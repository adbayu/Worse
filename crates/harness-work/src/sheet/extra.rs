//! Engine setup beyond cells and basic styles: defined names (workbook and sheet scope), row
//! heights, hidden rows/columns, italic/underline/wrap and border colors; plus small helpers.

use ironcalc::base::cell::CellValue;
use ironcalc::base::types::Color;
use ironcalc::base::{Model, ROW_HEIGHT_FACTOR};

use super::{hex_color, sheet_err, style_range};
use crate::error::WorkResult;
use crate::model::book::{parse_span, Book, Sheet};

/// Adds `book.names`; `Sheet!Name` keys are local to that sheet. Broken names are skipped so one
/// bad name (`#REF!`) never stops the workbook from loading.
pub(super) fn names(model: &mut Model<'static>, book: &Book) -> Vec<String> {
    let mut skipped = Vec::new();
    for (key, formula) in &book.names {
        let (scope, name) = match key.rsplit_once('!') {
            Some((sheet, name)) => {
                let idx = book
                    .sheets
                    .iter()
                    .position(|s| s.name.eq_ignore_ascii_case(sheet.trim_matches('\'')));
                (idx.and_then(|i| u32::try_from(i).ok()), name)
            }
            None => (None, key.as_str()),
        };
        // 引用型名称必须不带 `=`（IronCalc 按引用解析），LAMBDA 两种写法都接受。
        let raw = formula.trim().trim_start_matches('=');
        if model.new_defined_name(name, scope, raw).is_err() {
            skipped.push(key.clone());
        }
    }
    skipped
}

/// Row heights, hidden rows/columns, fonts and wrap of one sheet.
pub(super) fn layout(model: &mut Model<'static>, idx: u32, s: &Sheet) -> WorkResult<()> {
    for (row, pt) in &s.heights {
        if let Ok(r) = row.parse::<i32>() {
            model
                .set_row_height(idx, r, pt * ROW_HEIGHT_FACTOR)
                .map_err(sheet_err)?;
        }
    }
    for span in &s.hidden_rows {
        if let Some((a, b)) = parse_span(span, false) {
            for r in a..=b.min(a + 10_000) {
                model.set_row_hidden(idx, r, true).map_err(sheet_err)?;
            }
        }
    }
    for span in &s.hidden_cols {
        if let Some((a, b)) = parse_span(span, true) {
            for c in a..=b.min(a + 1_000) {
                model.set_column_hidden(idx, c, true).map_err(sheet_err)?;
            }
        }
    }
    for range in &s.italic {
        style_range(model, idx, range, |st| st.font.i = true)?;
    }
    for range in &s.underline {
        style_range(model, idx, range, |st| st.font.u = true)?;
    }
    for range in &s.wrap {
        style_range(model, idx, range, |st| {
            let mut a = st.alignment.clone().unwrap_or_default();
            a.wrap_text = true;
            st.alignment = Some(a);
        })?;
    }
    for (range, hex) in &s.border_colors {
        if let Some(color) = hex_color(hex) {
            style_range(model, idx, range, |st| {
                let b = &mut st.border;
                for item in [&mut b.top, &mut b.right, &mut b.bottom, &mut b.left]
                    .into_iter()
                    .flatten()
                {
                    item.color = color.clone();
                }
            })?;
        }
    }
    Ok(())
}

/// Whether a value satisfies a conditional rule like `>1000`, `<0`, `=done`, `<>x`.
pub(super) fn matches(value: &CellValue, when: &str) -> bool {
    let w = when.trim();
    let (op, rhs) = ["<=", ">=", "<>", "<", ">", "="]
        .iter()
        .find_map(|op| w.strip_prefix(op).map(|r| (*op, r.trim())))
        .unwrap_or(("=", w));
    match (value, rhs.parse::<f64>()) {
        (CellValue::Number(n), Ok(x)) => match op {
            "<=" => *n <= x,
            ">=" => *n >= x,
            "<>" => (*n - x).abs() > f64::EPSILON,
            "<" => *n < x,
            ">" => *n > x,
            _ => (*n - x).abs() < f64::EPSILON,
        },
        (CellValue::String(s), _) => match op {
            "<>" => !s.eq_ignore_ascii_case(rhs),
            "=" => s.eq_ignore_ascii_case(rhs),
            _ => false,
        },
        _ => false,
    }
}

pub(super) fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// `#RRGGBB` of the first colored border side, when it is not black.
pub(crate) fn border_color(st: &ironcalc::base::types::Style) -> Option<String> {
    let b = &st.border;
    [&b.top, &b.right, &b.bottom, &b.left]
        .into_iter()
        .flatten()
        .find_map(|item| match &item.color {
            Color::Rgb(h) if !h.eq_ignore_ascii_case("#000000") => Some(h.to_ascii_uppercase()),
            _ => None,
        })
}
