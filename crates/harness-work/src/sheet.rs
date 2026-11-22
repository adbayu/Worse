//! Sheet engine on `IronCalc`: `book.json` → evaluated model, formatted grid windows for the UI,
//! range values for charts, .xlsx and .csv output.

use ironcalc::base::cell::CellValue;
use ironcalc::base::types::{Color, MergedCell, SheetState};
use ironcalc::base::{Model, COLUMN_WIDTH_FACTOR, ROW_HEIGHT_FACTOR};

use crate::error::{WorkError, WorkResult};
use crate::model::book::{cell_input, column_name, parse_cell, parse_range, Book, Conditional};

pub struct SheetModel {
    model: Model<'static>,
    book: Book,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct GridCell {
    pub text: String,
    pub formula: String,
    pub number: bool,
    pub bold: bool,
    pub fill: Option<String>,
    pub error: bool,
    /// Font color `#RRGGBB` when set.
    pub color: Option<String>,
    /// `left`, `center`, `right` or empty (General).
    pub align: String,
    /// Border sides `t r b l`, upper case = thick.
    pub border: String,
    /// Font size in points, 0 = default.
    pub size: f64,
    pub italic: bool,
    pub underline: bool,
    pub wrap: bool,
    /// Border line color when not black.
    pub border_color: Option<String>,
    /// Note/comment text, if any.
    pub note: String,
}

/// A window of a sheet: `rows[r][c]` is cell (`first_row + r`, `first_col + c`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SheetGrid {
    pub sheet: String,
    pub first_row: i32,
    pub first_col: i32,
    pub rows: Vec<Vec<GridCell>>,
    /// Used area (last row, last column) of the whole sheet.
    pub used: (i32, i32),
    pub frozen: (i32, i32),
    /// Column widths in characters for the window's columns (0 = hidden).
    pub widths: Vec<f64>,
    /// Row heights in points for the window's rows (0 = hidden).
    pub heights: Vec<f64>,
}

fn sheet_err(e: impl std::fmt::Display) -> WorkError {
    WorkError::Sheet(e.to_string())
}

fn hex_color(hex: &str) -> Option<Color> {
    let h = hex.trim();
    (h.len() == 7 && h.starts_with('#') && h[1..].chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| Color::Rgb(h.to_ascii_uppercase()))
}

impl SheetModel {
    pub fn from_book(book: &Book) -> WorkResult<Self> {
        let mut model = Model::new_empty("book", locale::engine(&book.locale), "UTC", "en")
            .or_else(|_| Model::new_empty("book", "en", "UTC", "en"))
            .map_err(sheet_err)?;
        for (i, s) in book.sheets.iter().enumerate() {
            if i == 0 {
                let first = model
                    .workbook
                    .worksheets
                    .first()
                    .map(|w| w.name.clone())
                    .unwrap_or_default();
                if first != s.name {
                    model.rename_sheet(&first, &s.name).map_err(sheet_err)?;
                }
            } else {
                model.add_sheet(&s.name).map_err(sheet_err)?;
            }
        }
        // 修复：先注册已定义名称再写入公式单元格，否则引用名称的公式会被 IronCalc 解析为 #NAME?。
        extra::names(&mut model, book);
        for (i, s) in book.sheets.iter().enumerate() {
            let idx = u32::try_from(i).map_err(sheet_err)?;
            if s.hidden {
                // 隐藏工作表在导出的 .xlsx 中保持隐藏；失败（如全部隐藏）时保持可见。
                model.set_sheet_state(idx, SheetState::Hidden).ok();
            }
            for (key, value) in &s.cells {
                let Some((r, c)) = parse_cell(key) else {
                    continue;
                };
                model
                    .set_user_input(idx, r, c, cell_input(value))
                    .map_err(sheet_err)?;
            }
            for (col, w) in &s.widths {
                if let Some((_, c)) = parse_cell(&format!("{col}1")) {
                    model
                        .set_column_width(idx, c, w * COLUMN_WIDTH_FACTOR)
                        .map_err(sheet_err)?;
                }
            }
            for (range, fmt) in &s.formats {
                style_range(&mut model, idx, range, |st| st.num_fmt.clone_from(fmt))?;
            }
            for (range, fill) in &s.fills {
                if let Some(color) = hex_color(fill) {
                    style_range(&mut model, idx, range, |st| st.fill.color = color.clone())?;
                }
            }
            for range in &s.bold {
                style_range(&mut model, idx, range, |st| st.font.b = true)?;
            }
            style::apply(&mut model, idx, s)?;
            extra::layout(&mut model, idx, s)?;
            if let Some(ws) = model.workbook.worksheets.get_mut(i) {
                for m in &s.merges {
                    if let Some((r1, c1, r2, c2)) = parse_range(m) {
                        ws.merged_cells.push(MergedCell {
                            row: r1,
                            column: c1,
                            width: c2 - c1 + 1,
                            height: r2 - r1 + 1,
                        });
                    }
                }
            }
            if let Some((r, c)) = s.freeze.as_deref().and_then(parse_cell) {
                model.set_frozen_rows(idx, r - 1).map_err(sheet_err)?;
                model.set_frozen_columns(idx, c - 1).map_err(sheet_err)?;
            }
        }
        model.evaluate();
        Ok(Self {
            model,
            book: book.clone(),
        })
    }

    #[must_use]
    pub fn book(&self) -> &Book {
        &self.book
    }

    #[must_use]
    pub fn sheet_index(&self, name: &str) -> Option<u32> {
        let i = self
            .book
            .sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name.trim_matches('\'')))?;
        u32::try_from(i).ok()
    }

    /// Sets one cell (hand edit) and recalculates.
    pub fn set(&mut self, sheet: u32, row: i32, col: i32, input: &str) -> WorkResult<()> {
        self.model
            .set_user_input(sheet, row, col, input.to_string())
            .map_err(sheet_err)?;
        self.model.evaluate();
        Ok(())
    }

    #[must_use]
    pub fn formatted(&self, sheet: u32, row: i32, col: i32) -> String {
        let shown = self
            .model
            .get_formatted_cell_value(sheet, row, col)
            .unwrap_or_default();
        if matches!(self.value(sheet, row, col), CellValue::Number(_)) {
            locale::localize(&self.book.locale, &self.model.get_locale(), shown)
        } else {
            shown
        }
    }

    #[must_use]
    pub fn value(&self, sheet: u32, row: i32, col: i32) -> CellValue {
        self.model
            .get_cell_value_by_index(sheet, row, col)
            .unwrap_or(CellValue::None)
    }

    #[must_use]
    pub fn formula(&self, sheet: u32, row: i32, col: i32) -> Option<String> {
        self.model.get_cell_formula(sheet, row, col).ok().flatten()
    }

    /// Last used row and column of a sheet (from the source cells; at least 1×1).
    #[must_use]
    pub fn used(&self, sheet: u32) -> (i32, i32) {
        let Some(s) = usize::try_from(sheet)
            .ok()
            .and_then(|i| self.book.sheets.get(i))
        else {
            return (1, 1);
        };
        s.cells
            .keys()
            .filter_map(|k| parse_cell(k))
            .fold((1, 1), |(r, c), (r2, c2)| (r.max(r2), c.max(c2)))
    }

    /// Formatted values of a window, with styles and conditional fills applied.
    #[must_use]
    pub fn grid(
        &self,
        sheet: u32,
        first_row: i32,
        first_col: i32,
        rows: i32,
        cols: i32,
    ) -> SheetGrid {
        let src = usize::try_from(sheet)
            .ok()
            .and_then(|i| self.book.sheets.get(i));
        let mut out = SheetGrid {
            sheet: src.map(|s| s.name.clone()).unwrap_or_default(),
            first_row,
            first_col,
            used: self.used(sheet),
            frozen: (
                self.model.get_frozen_rows_count(sheet).unwrap_or(0),
                self.model.get_frozen_columns_count(sheet).unwrap_or(0),
            ),
            ..SheetGrid::default()
        };
        for c in first_col..first_col + cols {
            out.widths
                .push(if self.model.is_column_hidden(sheet, c).unwrap_or(false) {
                    0.0
                } else {
                    self.model.get_column_width(sheet, c).unwrap_or(90.0) / COLUMN_WIDTH_FACTOR
                });
        }
        for r in first_row..first_row + rows {
            out.heights
                .push(if self.model.is_row_hidden(sheet, r).unwrap_or(false) {
                    0.0
                } else {
                    self.model.get_row_height(sheet, r).unwrap_or(25.0) / ROW_HEIGHT_FACTOR
                });
        }
        for r in first_row..first_row + rows {
            let mut row = Vec::with_capacity(usize::try_from(cols).unwrap_or(0));
            for c in first_col..first_col + cols {
                row.push(self.cell(
                    sheet,
                    r,
                    c,
                    src.map(|s| s.conditional.as_slice()).unwrap_or_default(),
                ));
            }
            out.rows.push(row);
        }
        out
    }

    fn cell(&self, sheet: u32, r: i32, c: i32, conditional: &[Conditional]) -> GridCell {
        let value = self.value(sheet, r, c);
        let text = self.formatted(sheet, r, c);
        let style = self.model.get_style_for_cell(sheet, r, c).ok();
        let mut fill = style.as_ref().and_then(|s| match &s.fill.color {
            Color::Rgb(h) => Some(h.clone()),
            _ => None,
        });
        for cond in conditional {
            if let Some((r1, c1, r2, c2)) = parse_range(&cond.range) {
                if (r1..=r2).contains(&r)
                    && (c1..=c2).contains(&c)
                    && extra::matches(&value, &cond.when)
                {
                    fill = Some(cond.fill.clone());
                }
            }
        }
        let (color, align, border, size) = style.as_ref().map(style::look).unwrap_or_default();
        let note = usize::try_from(sheet)
            .ok()
            .and_then(|i| self.book.sheets.get(i))
            .and_then(|s| s.notes.get(&cell_name(r, c)).cloned())
            .unwrap_or_default();
        GridCell {
            error: text.starts_with('#') && text.ends_with(['!', '?', 'A']),
            number: matches!(value, CellValue::Number(_)),
            bold: style.as_ref().is_some_and(|s| s.font.b),
            italic: style.as_ref().is_some_and(|s| s.font.i),
            underline: style.as_ref().is_some_and(|s| s.font.u),
            wrap: style
                .as_ref()
                .and_then(|s| s.alignment.as_ref())
                .is_some_and(|a| a.wrap_text),
            border_color: style.as_ref().and_then(extra::border_color),
            note,
            color,
            align,
            border,
            size,
            formula: self.formula(sheet, r, c).unwrap_or_default(),
            text,
            fill,
        }
    }

    /// Values of a range like `Sheet1!A1:C5` (sheet defaults to the first one).
    pub fn range(&self, range: &str) -> WorkResult<Vec<Vec<CellValue>>> {
        let (sheet, cells) = match range.rsplit_once('!') {
            Some((s, c)) => (
                self.sheet_index(s)
                    .ok_or_else(|| WorkError::Invalid(format!("no sheet named {s}")))?,
                c,
            ),
            None => (0, range),
        };
        let (r1, c1, r2, c2) =
            parse_range(cells).ok_or_else(|| WorkError::Invalid(format!("bad range {range}")))?;
        Ok((r1..=r2)
            .map(|r| (c1..=c2).map(|c| self.value(sheet, r, c)).collect())
            .collect())
    }

    pub fn save_xlsx(&self, path: &std::path::Path) -> WorkResult<()> {
        let _ = std::fs::remove_file(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| WorkError::io(dir, e))?;
        }
        ironcalc::export::save_to_xlsx(&self.model, &path.to_string_lossy())
            .map_err(|e| WorkError::Export(format!("{e:?}")))
    }

    /// CSV of one sheet's used area (formatted values, RFC 4180 quoting).
    #[must_use]
    pub fn csv(&self, sheet: u32) -> String {
        let (rows, cols) = self.used(sheet);
        let mut out = String::new();
        for r in 1..=rows {
            let line: Vec<String> = (1..=cols)
                .map(|c| extra::csv_field(&self.formatted(sheet, r, c)))
                .collect();
            out.push_str(&line.join(","));
            out.push_str("\r\n");
        }
        out
    }

    #[must_use]
    pub fn model(&self) -> &Model<'static> {
        &self.model
    }
}

fn style_range(
    model: &mut Model<'static>,
    sheet: u32,
    range: &str,
    f: impl Fn(&mut ironcalc::base::types::Style),
) -> WorkResult<()> {
    let Some((r1, c1, r2, c2)) = parse_range(range) else {
        return Ok(());
    };
    // 防止整列格式写入上百万个单元格。
    if i64::from(r2 - r1 + 1) * i64::from(c2 - c1 + 1) > 200_000 {
        return Err(WorkError::Invalid(format!(
            "range {range} is too large to style"
        )));
    }
    for r in r1..=r2 {
        for c in c1..=c2 {
            let mut st = model.get_style_for_cell(sheet, r, c).map_err(sheet_err)?;
            f(&mut st);
            model.set_cell_style(sheet, r, c, &st).map_err(sheet_err)?;
        }
    }
    Ok(())
}

/// `A1`-style name of a cell.
#[must_use]
pub fn cell_name(row: i32, col: i32) -> String {
    format!("{}{row}", column_name(col))
}

pub(crate) mod edit;
mod extra;
pub(crate) mod locale;
pub(crate) mod refs;
pub(crate) mod style;
#[cfg(test)]
mod tests;
