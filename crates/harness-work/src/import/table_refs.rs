//! Rewrites Excel Table structured references (`Table1[Amount]`, `Table1[@Amount]`,
//! `Table1[[#Totals],[Amount]]`, `Table1[[Jan]:[Mar]]`) into A1 ranges. The engine only
//! understands them together with the table definitions, which `book.json` does not keep.

use std::collections::HashMap;

use ironcalc::base::types::Table as XlTable;
use serde_json::Value;

use crate::model::book::{column_name, parse_cell, parse_range, Book};

#[derive(Clone, Copy, PartialEq)]
enum Section {
    All,
    Data,
    Headers,
    Totals,
    ThisRow,
}

struct Table {
    name: String,
    /// `'Sheet'!` prefix of every rewritten reference.
    sheet: String,
    first_col: i32,
    columns: Vec<String>,
    header: Option<i32>,
    data: (i32, i32),
    totals: Option<i32>,
}

impl Table {
    fn new(t: &XlTable) -> Option<Self> {
        let (r1, c1, r2, _) = parse_range(&t.reference)?;
        let hdr = i32::try_from(t.header_row_count).unwrap_or(1);
        let tot = i32::try_from(t.totals_row_count).unwrap_or(0);
        let first = (r1 + hdr).min(r2);
        Some(Self {
            name: t.name.clone(),
            sheet: format!("'{}'!", t.sheet_name.replace('\'', "''")),
            first_col: c1,
            columns: t.columns.iter().map(|c| c.name.clone()).collect(),
            header: (hdr > 0).then_some(r1),
            data: (first, (r2 - tot).max(first)),
            totals: (tot > 0).then_some(r2),
        })
    }

    fn col(&self, name: &str) -> Option<i32> {
        let i = self
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name.trim()))?;
        Some(self.first_col + i32::try_from(i).ok()?)
    }

    /// A1 text for the bracket contents of `Table[inner]` in a formula on `row`.
    fn resolve(&self, inner: &str, row: i32) -> Option<String> {
        let mut section = Section::Data;
        let mut cols = Vec::new();
        for item in inner.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let item = item.strip_prefix('[').unwrap_or(item);
            if let Some(name) = item.strip_prefix('#') {
                section = match name.trim_end_matches(']').to_ascii_lowercase().as_str() {
                    "all" => Section::All,
                    "data" => Section::Data,
                    "headers" => Section::Headers,
                    "totals" => Section::Totals,
                    "this row" => Section::ThisRow,
                    _ => return None,
                };
                continue;
            }
            let item = match item.strip_prefix('@') {
                Some(rest) => {
                    section = Section::ThisRow;
                    rest
                }
                None => item,
            };
            for part in item.split(':') {
                cols.push(self.col(part.trim().trim_matches(['[', ']']))?);
            }
        }
        let last = self.first_col + i32::try_from(self.columns.len()).ok()? - 1;
        let (c1, c2) = match cols.as_slice() {
            [] => (self.first_col, last),
            [one] => (*one, *one),
            [a, .., b] => (*a, *b),
        };
        let (r1, r2) = match section {
            Section::All => (
                self.header.unwrap_or(self.data.0),
                self.totals.unwrap_or(self.data.1),
            ),
            Section::Data => self.data,
            Section::Headers => (self.header?, self.header?),
            Section::Totals => (self.totals?, self.totals?),
            Section::ThisRow => (row, row),
        };
        let abs = if section == Section::ThisRow { "" } else { "$" };
        let cell = |c: i32, r: i32| format!("${}{abs}{r}", column_name(c));
        Some(if (r1, c1) == (r2, c2) {
            format!("{}{}", self.sheet, cell(c1, r1))
        } else {
            format!("{}{}:{}", self.sheet, cell(c1, r1), cell(c2, r2))
        })
    }
}

/// Rewrites every structured reference in the formulas of `book`.
pub(super) fn rewrite(book: &mut Book, tables: &HashMap<String, XlTable>) {
    let tables: Vec<Table> = tables.values().filter_map(Table::new).collect();
    if tables.is_empty() {
        return;
    }
    for sheet in &mut book.sheets {
        for (coord, val) in &mut sheet.cells {
            let Some(f) = val
                .as_str()
                .filter(|s| s.starts_with('=') && s.contains('['))
            else {
                continue;
            };
            let row = parse_cell(coord).map_or(1, |(r, _)| r);
            let next = tables
                .iter()
                .fold(f.to_string(), |text, t| replace(&text, t, row));
            if next != f {
                *val = Value::String(next);
            }
        }
    }
}

/// Replaces each `Name[...]` of table `t` in `text`; unknown columns stay as written.
fn replace(text: &str, t: &Table, row: i32) -> String {
    let prefix = format!("{}[", t.name).to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = find(rest, &prefix) {
        let after = at + prefix.len();
        let Some(end) = closing(&rest[after..]).map(|e| after + e) else {
            break;
        };
        out.push_str(&rest[..at]);
        match t.resolve(&rest[after..end], row) {
            Some(a1) => out.push_str(&a1),
            None => out.push_str(&rest[at..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Start of the next `prefix` (lowercase) that is a whole name outside a string literal.
fn find(text: &str, prefix: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(prefix).map(|i| from + i) {
        let before = text[..i].chars().next_back();
        let in_string = text[..i].matches('"').count() % 2 == 1;
        if !in_string && !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
            return Some(i);
        }
        from = i + prefix.len();
    }
    None
}

/// Index of the `]` closing an already opened `[`, honoring nested brackets.
fn closing(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sales() -> Table {
        // B2:D10 with a header row and a totals row.
        Table {
            name: "Sales".into(),
            sheet: "'Data'!".into(),
            first_col: 2,
            columns: vec!["Region".into(), "Jan".into(), "Feb".into()],
            header: Some(2),
            data: (3, 9),
            totals: Some(10),
        }
    }

    #[test]
    fn rewrites_structured_refs() {
        let t = sales();
        let r = |s: &str| replace(s, &t, 5);
        assert_eq!(r("=SUM(Sales[Jan])"), "=SUM('Data'!$C$3:$C$9)");
        assert_eq!(r("=sales[@Feb]*2"), "='Data'!$D5*2");
        assert_eq!(r("=Sales[[#Totals],[Feb]]"), "='Data'!$D$10");
        assert_eq!(r("=SUM(Sales[[Jan]:[Feb]])"), "=SUM('Data'!$C$3:$D$9)");
        assert_eq!(r("=ROWS(Sales[#All])"), "=ROWS('Data'!$B$2:$D$10)");
        assert_eq!(
            r("=MySales[Jan]&\"Sales[Jan]\""),
            "=MySales[Jan]&\"Sales[Jan]\""
        );
        assert_eq!(r("=Sales[Missing]"), "=Sales[Missing]");
    }
}
