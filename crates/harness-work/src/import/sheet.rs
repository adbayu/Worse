//! `.xlsx` (through `IronCalc`) and `.csv` → `book.json`. Formulas, number formats, bold, fills,
//! column widths, merges and frozen panes are kept; charts, pivots and macros are reported.

use std::collections::BTreeMap;

use ironcalc::base::{Model, COLUMN_WIDTH_FACTOR};
use serde_json::Value;

use ironcalc::base::types::Worksheet;

use super::{audit, Imported};
use crate::error::{WorkError, WorkResult};
use crate::item::{to_json, Kind};
use crate::model::book::{column_name, Book, Sheet};
use crate::sheet::cell_name;
use crate::sheet::edit::source_value;

pub(crate) fn xlsx(bytes: &[u8], stem: &str) -> WorkResult<Imported> {
    read_xlsx(bytes, stem, true)
}

/// The same import without the audit (diff against the original).
pub(crate) fn xlsx_plain(bytes: &[u8], stem: &str) -> WorkResult<Imported> {
    read_xlsx(bytes, stem, false)
}

#[allow(clippy::too_many_lines)]
fn read_xlsx(bytes: &[u8], stem: &str, check: bool) -> WorkResult<Imported> {
    let wb = ironcalc::import::load_from_xlsx_bytes(bytes, stem, "en", "UTC")
        .map_err(|e| WorkError::Import(format!("{e:?}")))?;
    let model = Model::from_workbook(wb, "en").map_err(WorkError::Import)?;
    let mut book = Book {
        title: stem.to_string(),
        sheets: Vec::new(),
        ..Book::default()
    };
    let mut warnings = Vec::new();
    let mut used: Vec<(i32, i32)> = Vec::new();
    let mut cached: Vec<audit::Cached> = Vec::new();
    for props in model.get_worksheets_properties() {
        // 修复：Excel 中隐藏的工作表也被显示成标签页；保留数据（公式要用），只标记为隐藏。
        book.sheets.push(Sheet {
            hidden: !props.state.eq_ignore_ascii_case("visible"),
            name: props.name,
            ..Sheet::default()
        });
        used.push((0, 0));
    }
    let theme = model.get_theme();
    let base_size = usual_size(&model);
    for c in model.get_all_cells() {
        let Some(sheet) = book.sheets.get_mut(c.index as usize) else {
            continue;
        };
        let value = source_value(&model, c.index, c.row, c.column);
        if value
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|s| s.starts_with('='))
        {
            // 记录 Excel 保存的计算结果（模型尚未重新计算），用于导入后核对。
            if let Ok(v) = model.get_cell_value_by_index(c.index, c.row, c.column) {
                cached.push((c.index, c.row, c.column, v));
            }
        }
        let key = cell_name(c.row, c.column);
        // 修复：空单元格的填充色/边框（如黑色格子）也要导入，之前只导入有值的单元格。
        if let Ok(st) = model.get_style_for_cell(c.index, c.row, c.column) {
            crate::sheet::style::record(sheet, &key, &st, &theme, base_size);
            if value.is_some() {
                if !st.num_fmt.is_empty() && !st.num_fmt.eq_ignore_ascii_case("general") {
                    sheet.formats.insert(key.clone(), st.num_fmt.clone());
                }
                if st.font.b {
                    sheet.bold.push(key.clone());
                }
            }
        }
        let Some(v) = value else {
            continue;
        };
        sheet.cells.insert(key, v);
        if let Some(u) = used.get_mut(c.index as usize) {
            *u = (u.0.max(c.row), u.1.max(c.column));
        }
    }
    for (i, sheet) in book.sheets.iter_mut().enumerate() {
        let Ok(idx) = u32::try_from(i) else { continue };
        let default = model.get_column_width(idx, 16_000).unwrap_or(0.0);
        for col in 1..=used.get(i).map_or(0, |u| u.1).min(200) {
            let w = model.get_column_width(idx, col).unwrap_or(default);
            if (w - default).abs() > 0.5 {
                sheet.widths.insert(
                    column_name(col),
                    (w / COLUMN_WIDTH_FACTOR * 10.0).round() / 10.0,
                );
            }
        }
        if let Some(ws) = model.workbook.worksheets.get(i) {
            for m in &ws.merged_cells {
                sheet.merges.push(format!(
                    "{}:{}",
                    cell_name(m.row, m.column),
                    cell_name(m.row + m.height - 1, m.column + m.width - 1)
                ));
            }
        }
        if let Some(ws) = model.workbook.worksheets.get(i) {
            layout(sheet, ws);
        }
        crate::model::coalesce::sheet(sheet);
        let rows = model.get_frozen_rows_count(idx).unwrap_or(0);
        let cols = model.get_frozen_columns_count(idx).unwrap_or(0);
        if rows > 0 || cols > 0 {
            sheet.freeze = Some(cell_name(rows + 1, cols + 1));
        }
    }
    if book.sheets.is_empty() {
        book = Book {
            title: stem.to_string(),
            ..Book::default()
        };
    }
    names(&model, &mut book);
    super::table_refs::rewrite(&mut book, &model.workbook.tables);
    let flags = super::xlsx_extra::read(bytes, &mut book);
    book.date1904 = flags.date1904;
    book.iterate = flags.iterate;
    // 文件本身不存语言；从数字格式里的 [$-421] 等区域标记推断。
    if let Some(lang) = flags.locale {
        book.locale = lang.to_string();
    }
    let (checks, report) = if check {
        audit::run(&book, &cached, &flags)
    } else {
        (Vec::new(), String::new())
    };
    warnings.extend(checks);
    if contains(bytes, b"xl/charts/") {
        warnings.push("Charts in the workbook were not imported; ask the agent to recreate them from the data.".into());
    }
    if contains(bytes, b"xl/pivotTables/") {
        warnings.push("Pivot tables were imported as plain values.".into());
    }
    if contains(bytes, b"vbaProject.bin") {
        warnings.push("Macros were dropped.".into());
    }
    Ok(Imported {
        kind: Kind::Book,
        source: to_json(&book)?,
        assets: vec![("import-report.md".into(), report.into_bytes())],
        warnings,
    })
}

/// Row heights, hidden rows/columns and notes of an imported worksheet.
fn layout(sheet: &mut Sheet, ws: &Worksheet) {
    for row in &ws.rows {
        if row.hidden {
            sheet.hidden_rows.push(row.r.to_string());
        } else if row.custom_height && row.height > 0.0 {
            sheet
                .heights
                .insert(row.r.to_string(), (row.height * 10.0).round() / 10.0);
        }
    }
    for col in ws.cols.iter().filter(|c| c.hidden) {
        sheet.hidden_cols.push(if col.min == col.max {
            column_name(col.min)
        } else {
            format!(
                "{}:{}",
                column_name(col.min),
                column_name(col.max.min(16_384))
            )
        });
    }
    for note in &ws.comments {
        sheet
            .notes
            .insert(note.cell_ref.clone(), note.text.trim().to_string());
    }
}

/// Defined names (sheet-local ones as `Sheet!Name`) and Excel tables as names of their range.
fn names(model: &Model, book: &mut Book) {
    for (name, scope, formula) in model.get_defined_name_list() {
        if name.starts_with("_xlnm") || name.starts_with("_xl") {
            continue;
        }
        let key = match scope.and_then(|i| book.sheets.get(i as usize)) {
            Some(s) => format!("{}!{name}", s.name),
            None => name,
        };
        book.names.insert(key, formula);
    }
    for table in model.workbook.tables.values() {
        if !book.names.contains_key(&table.name) {
            book.names.insert(
                table.name.clone(),
                format!(
                    "'{}'!{}",
                    table.sheet_name.replace('\'', "''"),
                    table.reference
                ),
            );
        }
    }
}

/// The most used font size of the workbook; only other sizes are kept.
fn usual_size(model: &Model) -> i32 {
    let mut counts: BTreeMap<i32, usize> = BTreeMap::new();
    for c in model.get_all_cells() {
        if let Ok(st) = model.get_style_for_cell(c.index, c.row, c.column) {
            *counts.entry(st.font.sz).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map_or(crate::sheet::style::DEFAULT_SIZE, |(sz, _)| sz)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Splits one CSV line with quotes (`"a,b"`, `""` for a quote).
fn split_line(line: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == sep && !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

pub(crate) fn csv(text: &str, stem: &str, sep: char) -> WorkResult<Imported> {
    let mut cells = BTreeMap::new();
    for (r, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let Ok(row) = i32::try_from(r + 1) else { break };
        for (c, field) in split_line(line, sep).into_iter().enumerate() {
            let Ok(col) = i32::try_from(c + 1) else { break };
            let f = field.trim();
            if f.is_empty() {
                continue;
            }
            let v = match f.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
                Some(n) => Value::Number(n),
                None if f.starts_with('=') => Value::String(format!("'{f}")),
                None => Value::String(f.to_string()),
            };
            cells.insert(cell_name(row, col), v);
        }
    }
    let sheet = Sheet {
        name: "Sheet1".into(),
        cells,
        bold: vec!["A1:Z1".into()],
        freeze: Some("A2".into()),
        ..Sheet::default()
    };
    let book = Book {
        title: stem.to_string(),
        sheets: vec![sheet],
        ..Book::default()
    };
    Ok(Imported {
        kind: Kind::Book,
        source: to_json(&book)?,
        assets: Vec::new(),
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_csv() {
        assert_eq!(
            split_line(r#"a,"b,c","d ""x""""#, ','),
            vec!["a", "b,c", "d \"x\""]
        );
    }
}
