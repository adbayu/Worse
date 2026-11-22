//! `work_sheet` operations beyond read/find/set: errors, trace, fill, row/column insert/delete,
//! style and diff against the imported original.

use std::fmt::Write as _;

use serde_json::Value;

use crate::item::{Source, WorkItem};
use crate::model::book::{parse_cell, parse_range, parse_span, Book};
use crate::sheet::refs::refs;
use crate::sheet::{cell_name, SheetModel};

/// Splits `Sheet!A1` (quotes allowed) into sheet name and rest; the first sheet by default.
pub(super) fn split(model: &SheetModel, key: &str) -> (String, String) {
    match key.rsplit_once('!') {
        Some((s, c)) => (s.trim_matches('\'').to_string(), c.trim().to_string()),
        None => (
            model
                .book()
                .sheets
                .first()
                .map(|s| s.name.clone())
                .unwrap_or_default(),
            key.trim().to_string(),
        ),
    }
}

fn index(model: &SheetModel, sheet: &str) -> Result<u32, String> {
    model
        .sheet_index(sheet)
        .ok_or_else(|| format!("no sheet {sheet}; call work_sheet with read '*' for the list"))
}

fn sheet_name(model: &SheetModel, idx: u32) -> String {
    model
        .book()
        .sheets
        .get(idx as usize)
        .map(|s| s.name.clone())
        .unwrap_or_default()
}

/// `insert_rows`, `delete_rows`, `insert_cols`, `delete_cols`: `"Sheet!5:6"` / `"Sheet!C"`.
pub(super) fn structure(model: &mut SheetModel, args: &Value) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    for (key, rows, insert) in [
        ("insert_rows", true, true),
        ("delete_rows", true, false),
        ("insert_cols", false, true),
        ("delete_cols", false, false),
    ] {
        let Some(spec) = args.get(key).and_then(Value::as_str) else {
            continue;
        };
        let (sheet, span) = split(model, spec);
        let idx = index(model, &sheet)?;
        let (a, b) = parse_span(&span, !rows)
            .ok_or_else(|| format!("{key}: use 'Sheet!5:6' for rows or 'Sheet!C:D' for columns"))?;
        let n = b - a + 1;
        model
            .restructure(idx, rows, a, if insert { n } else { -n })
            .map_err(|e| e.to_string())?;
        done.push(format!("{key} {sheet}!{span}"));
    }
    Ok(done)
}

/// `fill: {"from": "Sheet!D2", "to": "D3:D20"}` copies the formula with relative references.
pub(super) fn fill(model: &mut SheetModel, args: &Value) -> Result<Vec<(u32, i32, i32)>, String> {
    let Some(spec) = args.get("fill") else {
        return Ok(Vec::new());
    };
    let from = spec.get("from").and_then(Value::as_str).unwrap_or_default();
    let to = spec.get("to").and_then(Value::as_str).unwrap_or_default();
    let (sheet, cell) = split(model, from);
    let idx = index(model, &sheet)?;
    let (r, c) = parse_cell(&cell).ok_or_else(|| format!("fill: bad from cell {from}"))?;
    let (r1, c1, r2, c2) = parse_range(to).ok_or_else(|| format!("fill: bad to range {to}"))?;
    if i64::from(r2 - r1 + 1) * i64::from(c2 - c1 + 1) > 20_000 {
        return Err("fill: range too large (max 20000 cells)".into());
    }
    let mut edits = Vec::new();
    for tr in r1..=r2 {
        for tc in c1..=c2 {
            let f = model
                .extend_to(idx, r, c, tr, tc)
                .map_err(|e| e.to_string())?;
            edits.push((idx, tr, tc, Value::String(f)));
        }
    }
    model.set_cells(&edits).map_err(|e| e.to_string())?;
    Ok(edits.into_iter().map(|(s, r, c, _)| (s, r, c)).collect())
}

/// `style: {"Sheet!A1:D1": {"bold": true, "fill": "#E5E7EB", "format": "#,##0", …}}`.
pub(super) fn style(model: &mut SheetModel, args: &Value) -> Result<usize, String> {
    let Some(map) = args.get("style").and_then(Value::as_object) else {
        return Ok(0);
    };
    let mut book: Book = model.book().clone();
    for (key, look) in map {
        let (sheet, range) = split(model, key);
        let idx = index(model, &sheet)? as usize;
        parse_range(&range).ok_or_else(|| format!("style: bad range {key}"))?;
        let s = book
            .sheets
            .get_mut(idx)
            .ok_or_else(|| format!("no sheet {sheet}"))?;
        let text = |k: &str| look.get(k).and_then(Value::as_str).map(str::to_string);
        let flag = |k: &str| look.get(k).and_then(Value::as_bool) == Some(true);
        if let Some(v) = text("format") {
            s.formats.insert(range.clone(), v);
        }
        if let Some(v) = text("fill") {
            s.fills.insert(range.clone(), v);
        }
        if let Some(v) = text("color") {
            s.colors.insert(range.clone(), v);
        }
        if let Some(v) = text("align") {
            s.align.insert(range.clone(), v);
        }
        if let Some(v) = text("border") {
            s.borders.insert(range.clone(), v);
        }
        if let Some(v) = look.get("size").and_then(Value::as_f64) {
            s.sizes.insert(range.clone(), v);
        }
        for (k, list) in [
            ("bold", &mut s.bold),
            ("italic", &mut s.italic),
            ("underline", &mut s.underline),
            ("wrap", &mut s.wrap),
        ] {
            if flag(k) {
                list.push(range.clone());
            }
        }
    }
    *model = SheetModel::from_book(&book).map_err(|e| e.to_string())?;
    Ok(map.len())
}

/// Every error cell with its formula and the first referenced cell that is itself an error.
pub(super) fn errors(model: &SheetModel) -> String {
    let list = model.errors();
    if list.is_empty() {
        return "No formula errors.\n".into();
    }
    let mut s = format!("{} formula error(s):\n", list.len());
    for (idx, r, c, shown, f) in list.iter().take(40) {
        let sheet = sheet_name(model, *idx);
        let cause = refs(f).into_iter().find_map(|rf| {
            let i = rf
                .sheet
                .as_deref()
                .map_or(Some(*idx), |n| model.sheet_index(n))?;
            let v = model.formatted(i, rf.r1, rf.c1);
            (v.starts_with('#')).then(|| format!(" ← {}={v}", cell_name(rf.r1, rf.c1)))
        });
        let _ = writeln!(
            s,
            "{sheet}!{}={shown} ({f}){}",
            cell_name(*r, *c),
            cause.unwrap_or_default()
        );
    }
    if list.len() > 40 {
        s.push_str("… more errors.\n");
    }
    s
}

/// Precedents (cells the formula reads) and dependents (formulas that read this cell).
#[allow(clippy::many_single_char_names)]
pub(super) fn trace(model: &SheetModel, target: &str) -> Result<String, String> {
    let (sheet, cell) = split(model, target);
    let idx = index(model, &sheet)?;
    let (r, c) = parse_cell(&cell).ok_or_else(|| format!("trace: bad cell {target}"))?;
    let mut s = format!("{sheet}!{}={}", cell_name(r, c), model.formatted(idx, r, c));
    match model.formula(idx, r, c) {
        Some(f) => {
            let _ = writeln!(s, " ({f})\nReads:");
            for rf in refs(&f).iter().take(30) {
                let i = rf
                    .sheet
                    .as_deref()
                    .map_or(Some(idx), |n| model.sheet_index(n));
                let Some(i) = i else { continue };
                let name = sheet_name(model, i);
                if (rf.r1, rf.c1) == (rf.r2, rf.c2) {
                    let v = model.formatted(i, rf.r1, rf.c1);
                    let sub = model
                        .formula(i, rf.r1, rf.c1)
                        .map(|f| format!(" ({f})"))
                        .unwrap_or_default();
                    let _ = writeln!(s, "- {name}!{}={v}{sub}", cell_name(rf.r1, rf.c1));
                } else {
                    let _ = writeln!(
                        s,
                        "- {name}!{}:{} (range)",
                        cell_name(rf.r1, rf.c1),
                        cell_name(rf.r2, rf.c2)
                    );
                }
            }
        }
        None => s.push_str(" (value)\n"),
    }
    s.push_str("Read by:\n");
    let mut n = 0;
    for (i, sh) in model.book().sheets.iter().enumerate() {
        let Ok(si) = u32::try_from(i) else { continue };
        for (key, v) in &sh.cells {
            let Some(f) = v.as_str().filter(|f| f.starts_with('=')) else {
                continue;
            };
            let hit = refs(f).iter().any(|rf| {
                let on = rf
                    .sheet
                    .as_deref()
                    .map_or(si == idx, |n| model.sheet_index(n) == Some(idx));
                on && rf.contains(r, c)
            });
            if hit && n < 30 {
                let _ = writeln!(s, "- {}!{key} ({f})", sh.name);
                n += 1;
            }
        }
    }
    if n == 0 {
        s.push_str("- nothing\n");
    }
    Ok(s)
}

/// Cells that differ from the imported original workbook.
pub(super) fn diff(item: &WorkItem, model: &SheetModel) -> String {
    let Some(Source::Book(original)) = crate::import::original(item) else {
        return "No original workbook to compare with (only imported .xlsx items have one).\n"
            .into();
    };
    let mut lines = Vec::new();
    for sh in &model.book().sheets {
        let before = original.sheets.iter().find(|o| o.name == sh.name);
        let empty = std::collections::BTreeMap::new();
        let old = before.map_or(&empty, |o| &o.cells);
        for (k, v) in &sh.cells {
            if old.get(k) != Some(v) {
                let was = old
                    .get(k)
                    .map_or_else(|| "(empty)".into(), Value::to_string);
                lines.push(format!("{}!{k}: {was} → {v}", sh.name));
            }
        }
        for k in old.keys().filter(|k| !sh.cells.contains_key(*k)) {
            lines.push(format!("{}!{k}: cleared", sh.name));
        }
    }
    if lines.is_empty() {
        return "No cell changes since import.\n".into();
    }
    let mut s = format!("{} cell(s) changed since import:\n", lines.len());
    for l in lines.iter().take(60) {
        let _ = writeln!(s, "{l}");
    }
    if lines.len() > 60 {
        s.push_str("… more changes.\n");
    }
    s
}

/// Header-aware / form-style `label → value` formatting for one range.
pub(super) fn pairs(
    model: &SheetModel,
    idx: u32,
    r1: i32,
    c1: i32,
    r_end: i32,
    c_end: i32,
) -> String {
    let clip = |s: &str, max: usize| {
        if s.chars().count() <= max {
            s.to_string()
        } else {
            format!("{}…", s.chars().take(max).collect::<String>())
        }
    };
    let mut out = String::new();
    let first_non_empty =
        (r1..=r_end).find(|r| (c1..=c_end).any(|c| !model.formatted(idx, *r, c).is_empty()));
    let headers: Vec<(i32, String)> = first_non_empty
        .map(|hr| {
            let cells: Vec<(i32, String)> = (c1..=c_end)
                .filter_map(|c| {
                    let v = model.formatted(idx, hr, c);
                    (!v.is_empty()).then_some((c, v))
                })
                .collect();
            let all_text =
                cells.len() >= 2 && cells.iter().all(|(_, v)| v.trim().parse::<f64>().is_err());
            if all_text {
                cells
            } else {
                Vec::new()
            }
        })
        .unwrap_or_default();

    if !headers.is_empty() {
        let start = first_non_empty.map_or(r1, |r| r + 1);
        for r in start..=r_end {
            let parts: Vec<String> = headers
                .iter()
                .filter_map(|(c, h)| {
                    let v = model.formatted(idx, r, *c);
                    (!v.is_empty()).then(|| format!("{} → {}", clip(h, 24), clip(&v, 48)))
                })
                .collect();
            if !parts.is_empty() {
                let _ = writeln!(out, "row {r}: {}", parts.join(" | "));
            }
        }
        return out;
    }
    for r in r1..=r_end {
        let filled: Vec<(i32, String)> = (c1..=c_end)
            .filter_map(|c| {
                let v = model.formatted(idx, r, c);
                (!v.is_empty()).then_some((c, v))
            })
            .collect();
        if filled.is_empty() {
            continue;
        }
        let mut parts = Vec::new();
        let mut i = 0;
        while i < filled.len() {
            let (col, ref txt) = filled[i];
            if i + 1 < filled.len()
                && txt.trim().parse::<f64>().is_err()
                && filled[i + 1].0 <= col + 2
            {
                let (vcol, ref val) = filled[i + 1];
                parts.push(format!(
                    "{} → {}={}",
                    clip(txt, 28),
                    cell_name(r, vcol),
                    clip(val, 40)
                ));
                i += 2;
            } else {
                parts.push(format!("{}={}", cell_name(r, col), clip(txt, 48)));
                i += 1;
            }
        }
        let _ = writeln!(out, "row {r}: {}", parts.join(" | "));
    }
    out
}
