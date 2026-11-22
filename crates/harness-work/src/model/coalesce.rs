//! Turns per-cell style maps into rectangles: equal neighbours in a row become a run, equal runs
//! in consecutive rows become one range. Imported workbooks shrink 10–30× this way.

use std::collections::BTreeMap;

use super::book::{parse_cell, Sheet};
use crate::sheet::cell_name;

/// Rectangles `(r1, c1, r2, c2)` covering `cells` exactly.
fn rects(mut cells: Vec<(i32, i32)>) -> Vec<(i32, i32, i32, i32)> {
    cells.sort_unstable();
    cells.dedup();
    // Row runs.
    let mut runs: Vec<(i32, i32, i32)> = Vec::new();
    for (r, c) in cells {
        match runs.last_mut() {
            Some((rr, _, c2)) if *rr == r && *c2 + 1 == c => *c2 = c,
            _ => runs.push((r, c, c)),
        }
    }
    // Stack equal runs of consecutive rows.
    let mut open: BTreeMap<(i32, i32), (i32, i32)> = BTreeMap::new();
    let mut out = Vec::new();
    for (r, c1, c2) in runs {
        match open.get_mut(&(c1, c2)) {
            Some((_, r2)) if *r2 + 1 == r => *r2 = r,
            _ => {
                if let Some((r1, r2)) = open.insert((c1, c2), (r, r)) {
                    out.push((r1, c1, r2, c2));
                }
            }
        }
    }
    out.extend(
        open.into_iter()
            .map(|((c1, c2), (r1, r2))| (r1, c1, r2, c2)),
    );
    out.sort_unstable();
    out
}

fn name(r1: i32, c1: i32, r2: i32, c2: i32) -> String {
    if (r1, c1) == (r2, c2) {
        cell_name(r1, c1)
    } else {
        format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
    }
}

/// Merges single-cell keys with equal values; range keys stay as they are (applied first).
pub(crate) fn map<T: Clone + PartialEq>(src: &BTreeMap<String, T>) -> BTreeMap<String, T> {
    let mut groups: Vec<(T, Vec<(i32, i32)>)> = Vec::new();
    let mut out = BTreeMap::new();
    for (key, value) in src {
        let Some(cell) = (!key.contains(':')).then(|| parse_cell(key)).flatten() else {
            out.insert(key.clone(), value.clone());
            continue;
        };
        match groups.iter_mut().find(|(v, _)| v == value) {
            Some((_, cells)) => cells.push(cell),
            None => groups.push((value.clone(), vec![cell])),
        }
    }
    for (value, cells) in groups {
        for (r1, c1, r2, c2) in rects(cells) {
            out.insert(name(r1, c1, r2, c2), value.clone());
        }
    }
    out
}

/// Same for a list of ranges (bold, italic, …).
pub(crate) fn list(src: &[String]) -> Vec<String> {
    let mut cells = Vec::new();
    let mut out = Vec::new();
    for key in src {
        match (!key.contains(':')).then(|| parse_cell(key)).flatten() {
            Some(cell) => cells.push(cell),
            None => out.push(key.clone()),
        }
    }
    out.extend(
        rects(cells)
            .into_iter()
            .map(|(r1, c1, r2, c2)| name(r1, c1, r2, c2)),
    );
    out
}

/// Coalesces every style map of a sheet.
pub(crate) fn sheet(s: &mut Sheet) {
    s.formats = map(&s.formats);
    s.fills = map(&s.fills);
    s.colors = map(&s.colors);
    s.align = map(&s.align);
    s.borders = map(&s.borders);
    s.border_colors = map(&s.border_colors);
    s.sizes = map(&s.sizes);
    s.bold = list(&s.bold);
    s.italic = list(&s.italic);
    s.underline = list(&s.underline);
    s.wrap = list(&s.wrap);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_blocks() {
        let mut m = BTreeMap::new();
        for r in 1..=3 {
            for c in ["A", "B", "C"] {
                m.insert(format!("{c}{r}"), "x".to_string());
            }
        }
        m.insert("D1".into(), "y".into());
        m.insert("E5:F6".into(), "x".into());
        let out = map(&m);
        assert_eq!(out.get("A1:C3").map(String::as_str), Some("x"));
        assert_eq!(out.get("D1").map(String::as_str), Some("y"));
        assert_eq!(out.len(), 3);
        assert_eq!(
            list(&["A1".into(), "B1".into(), "A2".into()]),
            vec!["A1:B1".to_string(), "A2".to_string()]
        );
    }
}
