//! Moves range-keyed sheet data when rows or columns are inserted (`n > 0`) or deleted
//! (`n < 0`) at `at`. Cell values are regenerated from the engine; this handles the rest.

use std::collections::BTreeMap;

use super::book::{column_name, parse_range, parse_span, Sheet};
use crate::sheet::cell_name;

/// New first/last index of a span, `None` when the span was deleted entirely.
fn span(a: i32, b: i32, at: i32, n: i32) -> Option<(i32, i32)> {
    if n >= 0 {
        let f = |x: i32| if x >= at { x + n } else { x };
        return Some((f(a), f(b)));
    }
    let end = at - n - 1;
    let lo = if a < at {
        a
    } else if a > end {
        a + n
    } else {
        at
    };
    let hi = if b < at {
        b
    } else if b > end {
        b + n
    } else {
        at - 1
    };
    (hi >= lo).then_some((lo, hi))
}

/// Shifts `A1` / `A1:C3` along rows (`rows = true`) or columns.
pub(crate) fn range(key: &str, rows: bool, at: i32, n: i32) -> Option<String> {
    let (r1, c1, r2, c2) = parse_range(key)?;
    let (r1, c1, r2, c2) = if rows {
        let (a, b) = span(r1, r2, at, n)?;
        (a, c1, b, c2)
    } else {
        let (a, b) = span(c1, c2, at, n)?;
        (r1, a, r2, b)
    };
    Some(if key.contains(':') {
        format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
    } else {
        cell_name(r1, c1)
    })
}

fn map<T: Clone>(m: &BTreeMap<String, T>, rows: bool, at: i32, n: i32) -> BTreeMap<String, T> {
    m.iter()
        .filter_map(|(k, v)| range(k, rows, at, n).map(|k| (k, v.clone())))
        .collect()
}

fn list(v: &[String], rows: bool, at: i32, n: i32) -> Vec<String> {
    v.iter().filter_map(|k| range(k, rows, at, n)).collect()
}

/// Shifts a `5:9` / `C:E` span list on its own axis.
fn spans(v: &[String], columns: bool, at: i32, n: i32) -> Vec<String> {
    v.iter()
        .filter_map(|s| {
            let (a, b) = parse_span(s, columns)?;
            let (a, b) = span(a, b, at, n)?;
            let name = |x: i32| {
                if columns {
                    column_name(x)
                } else {
                    x.to_string()
                }
            };
            Some(if a == b {
                name(a)
            } else {
                format!("{}:{}", name(a), name(b))
            })
        })
        .collect()
}

/// Applies a row (`rows = true`) or column insert/delete to everything but the cells.
pub(crate) fn sheet(s: &mut Sheet, rows: bool, at: i32, n: i32) {
    s.formats = map(&s.formats, rows, at, n);
    s.fills = map(&s.fills, rows, at, n);
    s.colors = map(&s.colors, rows, at, n);
    s.align = map(&s.align, rows, at, n);
    s.borders = map(&s.borders, rows, at, n);
    s.border_colors = map(&s.border_colors, rows, at, n);
    s.sizes = map(&s.sizes, rows, at, n);
    s.notes = map(&s.notes, rows, at, n);
    s.links = map(&s.links, rows, at, n);
    s.validation = map(&s.validation, rows, at, n);
    s.bold = list(&s.bold, rows, at, n);
    s.italic = list(&s.italic, rows, at, n);
    s.underline = list(&s.underline, rows, at, n);
    s.wrap = list(&s.wrap, rows, at, n);
    s.merges = list(&s.merges, rows, at, n);
    s.conditional
        .retain_mut(|c| match range(&c.range, rows, at, n) {
            Some(r) => {
                c.range = r;
                true
            }
            None => false,
        });
    for chart in &mut s.charts {
        if let Some(a) = range(&chart.anchor, rows, at, n) {
            chart.anchor = a;
        }
    }
    if rows {
        s.hidden_rows = spans(&s.hidden_rows, false, at, n);
        s.heights = s
            .heights
            .iter()
            .filter_map(|(k, v)| {
                let r: i32 = k.parse().ok()?;
                span(r, r, at, n).map(|(r, _)| (r.to_string(), *v))
            })
            .collect();
    } else {
        s.hidden_cols = spans(&s.hidden_cols, true, at, n);
        s.widths = s
            .widths
            .iter()
            .filter_map(|(k, v)| {
                let (c, _) = parse_span(k, true)?;
                span(c, c, at, n).map(|(c, _)| (column_name(c), *v))
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_ranges() {
        assert_eq!(range("A5:B9", true, 3, 2).as_deref(), Some("A7:B11"));
        assert_eq!(range("A5:B9", true, 6, -2).as_deref(), Some("A5:B7"));
        assert_eq!(range("A6", true, 6, -2), None);
        assert_eq!(range("C1", false, 2, 1).as_deref(), Some("D1"));
        assert_eq!(range("A1", false, 2, 1).as_deref(), Some("A1"));
    }
}
