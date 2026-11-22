//! `book.json` (`harness-book/1`): sheets with `A1`-keyed cells. A cell value is a number, a bool,
//! a string, or a formula string starting with `=`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::chart::Chart;

pub const FORMAT: &str = "harness-book/1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Conditional {
    /// Range like `E2:E20`.
    pub range: String,
    /// Comparison like `>1000`, `<0`, `=done`.
    pub when: String,
    pub fill: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SheetChart {
    #[serde(default)]
    pub id: String,
    /// Top-left cell of the chart (`G2`).
    pub anchor: String,
    pub chart: Chart,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Sheet {
    pub name: String,
    #[serde(default)]
    pub cells: BTreeMap<String, Value>,
    /// Number formats by range: `{"C2:C9": "#,##0.00"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub formats: BTreeMap<String, String>,
    /// Column widths in characters: `{"A": 24}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub widths: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merges: Vec<String>,
    /// First unfrozen cell, `B2` freezes row 1 and column A.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze: Option<String>,
    /// Background fill by range: `{"A1:E1": "#e5e7eb"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fills: BTreeMap<String, String>,
    /// Bold ranges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bold: Vec<String>,
    /// Font colors by range: `{"A4": "#FF0000"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: BTreeMap<String, String>,
    /// Horizontal alignment by range: `left`, `center` or `right`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub align: BTreeMap<String, String>,
    /// Borders by range: sides `t r b l`, upper case for a thick line (`"TrbL"`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub borders: BTreeMap<String, String>,
    /// Font sizes in points by range.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sizes: BTreeMap<String, f64>,
    /// Italic, underlined and wrapped ranges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub italic: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub underline: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wrap: Vec<String>,
    /// Border line colors by range (`#RRGGBB`; black when absent).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub border_colors: BTreeMap<String, String>,
    /// Row heights in points: `{"3": 30}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub heights: BTreeMap<String, f64>,
    /// Hidden rows (`"5"`, `"5:9"`) and columns (`"C"`, `"C:E"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_rows: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_cols: Vec<String>,
    /// Cell notes/comments: `{"B4": "check with finance"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, String>,
    /// Hyperlinks: `{"A9": "https://example.com"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub links: BTreeMap<String, String>,
    /// Dropdown lists by range: items, or one `=Sheet!A1:A9` source.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub validation: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditional: Vec<Conditional>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub charts: Vec<SheetChart>,
    /// Hidden in Excel: kept for formulas, not shown as a tab.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Book {
    pub format: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub sheets: Vec<Sheet>,
    /// Named ranges: `{"Total": "Sheet1!$E$10"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, String>,
    /// Workbook display locale (e.g. `"en"`, `"id"`, `"de"`). Empty = `"en"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub locale: String,
    /// 1904 date system flag (Excel for Mac).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub date1904: bool,
    /// Iterative calculation flag for circular references.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub iterate: bool,
}

impl Default for Book {
    fn default() -> Self {
        Self {
            format: FORMAT.into(),
            title: String::new(),
            sheets: vec![Sheet {
                name: "Sheet1".into(),
                ..Sheet::default()
            }],
            names: BTreeMap::new(),
            locale: String::new(),
            date1904: false,
            iterate: false,
        }
    }
}

/// A cell as typed into the engine (`=SUM(A1:A3)`, `12.5`, `TRUE`, `text`).
#[must_use]
pub fn cell_input(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `A1` → (row 1, column 1). Accepts `$` markers. Rows and columns start at 1.
#[must_use]
pub fn parse_cell(cell: &str) -> Option<(i32, i32)> {
    let cell = cell.replace('$', "");
    let split = cell.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = cell.split_at(split);
    if letters.is_empty() || !letters.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut col = 0i32;
    for c in letters.chars() {
        col = col.checked_mul(26)? + i32::from(c.to_ascii_uppercase() as u8 - b'A' + 1);
    }
    let row: i32 = digits.parse().ok()?;
    (row >= 1 && col >= 1 && row <= 1_048_576 && col <= 16_384).then_some((row, col))
}

/// `A1:C3` or `B2` → (first row, first col, last row, last col).
#[must_use]
pub fn parse_range(range: &str) -> Option<(i32, i32, i32, i32)> {
    let range = range.rsplit('!').next().unwrap_or(range);
    match range.split_once(':') {
        Some((a, b)) => {
            let (r1, c1) = parse_cell(a)?;
            let (r2, c2) = parse_cell(b)?;
            Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
        }
        None => parse_cell(range).map(|(r, c)| (r, c, r, c)),
    }
}

/// `5`, `5:9`, `C` or `C:E` → inclusive (first, last) row or column numbers.
#[must_use]
pub fn parse_span(span: &str, columns: bool) -> Option<(i32, i32)> {
    let one = |s: &str| -> Option<i32> {
        let s = s.trim().replace('$', "");
        if columns {
            parse_cell(&format!("{s}1")).map(|(_, c)| c)
        } else {
            s.parse().ok().filter(|r| *r >= 1)
        }
    };
    match span.split_once(':') {
        Some((a, b)) => {
            let (x, y) = (one(a)?, one(b)?);
            Some((x.min(y), x.max(y)))
        }
        None => one(span).map(|x| (x, x)),
    }
}

/// Column number (1-based) → letters.
#[must_use]
pub fn column_name(mut col: i32) -> String {
    let mut out = Vec::new();
    while col > 0 {
        let rem = u8::try_from((col - 1) % 26).unwrap_or(0);
        out.push(char::from(b'A' + rem));
        col = (col - 1) / 26;
    }
    out.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_refs_round_trip() {
        assert_eq!(parse_cell("A1"), Some((1, 1)));
        assert_eq!(parse_cell("$AB$12"), Some((12, 28)));
        assert_eq!(column_name(28), "AB");
        assert_eq!(parse_range("Sheet1!C3:A1"), Some((1, 1, 3, 3)));
        assert_eq!(parse_cell("1A"), None);
    }
}
