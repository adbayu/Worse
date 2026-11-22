//! `work_sheet` tool: sheet overview, find, read and edits on `book.json` through the formula
//! engine, so agents never need to open the (often huge) source file of an imported workbook.
//! The evaluated model is cached between calls while the source files are unchanged.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::SystemTime;

use serde_json::Value;

use super::sheet_ops::{self, split};
use super::{arg, text, Server};
use crate::item::{Source, WorkItem};
use crate::model::book::{parse_cell, parse_range};
use crate::sheet::{cell_name, SheetModel};

/// Rows and columns returned by one read.
const MAX_ROWS: i32 = 60;
const MAX_COLS: i32 = 26;
/// Hits returned by one find.
const MAX_HITS: usize = 25;
/// Cells shown as context of a found cell's row.
const ROW_CONTEXT: usize = 10;
/// Blocks listed per sheet in the overview.
const MAX_BLOCKS: usize = 6;

type Stamp = (Option<SystemTime>, Option<SystemTime>);

pub(crate) struct Cached {
    dir: PathBuf,
    stamp: Stamp,
    model: SheetModel,
}

fn stamp(item: &WorkItem) -> Stamp {
    let m = |p: PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    (
        m(item.source_path()),
        m(item.dir.join(crate::model::look::FILE)),
    )
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// `H5=30000` or `H7=40900 (=H5+H6)`; `mode` `formulas` shows `H7: =H5+H6`, `values` no formula.
fn cell_text(model: &SheetModel, sheet: u32, r: i32, c: i32, mode: &str) -> Option<String> {
    let shown = model.formatted(sheet, r, c);
    let formula = model.formula(sheet, r, c);
    if shown.is_empty() && formula.is_none() {
        return None;
    }
    let name = cell_name(r, c);
    Some(match (mode, formula) {
        ("formulas", Some(f)) => format!("{name}: {}", clip(&f, 80)),
        ("values", _) | (_, None) => format!("{name}={}", clip(&shown, 60)),
        (_, Some(f)) => format!("{name}={} ({})", clip(&shown, 60), clip(&f, 60)),
    })
}

/// Non-empty cells of one row within `c1..=c2`, joined compactly.
fn row_text(
    m: &SheetModel,
    sheet: u32,
    r: i32,
    c1: i32,
    c2: i32,
    max: usize,
    mode: &str,
) -> String {
    (c1..=c2)
        .filter_map(|c| cell_text(m, sheet, r, c, mode))
        .take(max)
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Sheets with used range and blocks of consecutive filled rows with their first labels.
fn overview(model: &SheetModel) -> String {
    let mut s = String::from("Sheets (name: used range; blocks of filled rows):\n");
    for (i, sh) in model.book().sheets.iter().enumerate() {
        let idx = u32::try_from(i).unwrap_or(0);
        let (rows, cols) = model.used(idx);
        let hidden = if sh.hidden { " (hidden)" } else { "" };
        let _ = writeln!(s, "- {}{hidden}: A1:{}", sh.name, cell_name(rows, cols));
        let mut filled: Vec<i32> = sh
            .cells
            .keys()
            .filter_map(|k| parse_cell(k))
            .map(|(r, _)| r)
            .collect();
        filled.sort_unstable();
        filled.dedup();
        let mut blocks: Vec<(i32, i32)> = Vec::new();
        for r in filled {
            match blocks.last_mut() {
                Some((_, end)) if *end + 1 >= r => *end = r,
                _ => blocks.push((r, r)),
            }
        }
        for (a, b) in blocks.iter().take(MAX_BLOCKS) {
            let labels: Vec<String> = (1..=cols)
                .filter_map(|c| {
                    let v = model.formatted(idx, *a, c);
                    (!v.is_empty() && v.parse::<f64>().is_err()).then(|| clip(&v, 24))
                })
                .take(6)
                .collect();
            let _ = writeln!(s, "  rows {a}–{b}: {}", labels.join(" | "));
        }
        if blocks.len() > MAX_BLOCKS {
            let _ = writeln!(s, "  … {} more blocks", blocks.len() - MAX_BLOCKS);
        }
    }
    s.push_str("Use find to locate a label or value, then read 'Sheet!A1:H20' or set cells.\n");
    s
}

fn find(model: &SheetModel, needle: &str, exact: bool, only: &str, scope: &str) -> String {
    let needle = needle.to_lowercase();
    let mut hits = Vec::new();
    'sheets: for (i, sh) in model.book().sheets.iter().enumerate() {
        if !only.is_empty() && !sh.name.eq_ignore_ascii_case(only) {
            continue;
        }
        let idx = u32::try_from(i).unwrap_or(0);
        let mut cells: Vec<(i32, i32, String)> = sh
            .cells
            .iter()
            .filter_map(|(k, v)| {
                let raw = v.as_str().map_or_else(|| v.to_string(), str::to_string);
                parse_cell(k).map(|(r, c)| (r, c, raw.to_lowercase()))
            })
            .collect();
        cells.sort_unstable();
        for (r, c, raw) in cells {
            let is_formula = raw.starts_with('=');
            let shown = model.formatted(idx, r, c).to_lowercase();
            let hit = match scope {
                "formulas" => {
                    is_formula
                        && if exact {
                            raw.trim() == needle
                        } else {
                            raw.contains(&needle)
                        }
                }
                "labels" => {
                    !is_formula
                        && raw.trim().parse::<f64>().is_err()
                        && if exact {
                            shown.trim() == needle
                        } else {
                            shown.contains(&needle)
                        }
                }
                "values" => {
                    if exact {
                        shown.trim() == needle || (!is_formula && raw.trim() == needle)
                    } else {
                        shown.contains(&needle) || (!is_formula && raw.contains(&needle))
                    }
                }
                _ => {
                    if exact {
                        raw.trim() == needle || shown.trim() == needle
                    } else {
                        raw.contains(&needle) || shown.contains(&needle)
                    }
                }
            };
            if hit {
                hits.push((sh.name.clone(), idx, r, c));
                if hits.len() > MAX_HITS {
                    break 'sheets;
                }
            }
        }
    }
    if hits.is_empty() {
        return format!("No cell contains \"{needle}\".\n");
    }
    let mut s = String::new();
    for (name, idx, r, c) in hits.iter().take(MAX_HITS) {
        let (_, cols) = model.used(*idx);
        let _ = writeln!(
            s,
            "{name}!{} — row {r}: {}",
            cell_name(*r, *c),
            row_text(model, *idx, *r, 1, cols, ROW_CONTEXT, "")
        );
    }
    if hits.len() > MAX_HITS {
        let _ = writeln!(s, "… more matches; narrow the search (exact, sheet, in).");
    }
    s
}

/// Header-aware / form-style `label → value` formatting for one range.
fn read(model: &SheetModel, read: &str, mode: &str) -> Result<String, String> {
    // A bare sheet name reads that sheet's used range.
    let (sheet, range) = match model.sheet_index(read) {
        Some(i) => {
            let (rows, cols) = model.used(i);
            (read.to_string(), format!("A1:{}", cell_name(rows, cols)))
        }
        None => split(model, read),
    };
    let idx = model
        .sheet_index(&sheet)
        .ok_or_else(|| format!("no sheet {sheet}; call work_sheet with read '*' for the list"))?;
    let (r1, c1, r2, c2) = parse_range(&range)
        .ok_or_else(|| format!("bad range {range}; use 'Sheet!A1:H20', a sheet name or '*'"))?;
    let (r_end, c_end) = (r2.min(r1 + MAX_ROWS - 1), c2.min(c1 + MAX_COLS - 1));
    let label = match mode {
        "formulas" => "formulas",
        "pairs" => "label → value pairs",
        _ => "computed",
    };
    let mut out = format!(
        "{sheet}!{}:{} ({label}, empty cells omitted):\n",
        cell_name(r1, c1),
        cell_name(r_end, c_end),
    );
    if mode == "pairs" {
        out.push_str(&sheet_ops::pairs(model, idx, r1, c1, r_end, c_end));
    } else {
        for r in r1..=r_end {
            let row = row_text(model, idx, r, c1, c_end, usize::MAX, mode);
            if !row.is_empty() {
                let _ = writeln!(out, "{row}");
            }
        }
    }
    if r_end < r2 || c_end < c2 {
        let _ = writeln!(out, "… truncated; read a smaller range for the rest.");
    }
    Ok(out)
}

impl Server {
    pub(super) fn sheet(&self, args: &Value) -> Result<Value, String> {
        let item = self.item(arg(args, "path"))?;
        if item.kind != crate::item::Kind::Book {
            return Err("work_sheet works on sheets (book.json) only".into());
        }
        let mut slot = self.sheets.borrow_mut();
        let now = stamp(&item);
        if !slot
            .as_ref()
            .is_some_and(|c| c.dir == item.dir && c.stamp == now)
        {
            let Source::Book(book) = item.load().map_err(|e| e.to_string())? else {
                return Err("work_sheet works on sheets (book.json) only".into());
            };
            let model = SheetModel::from_book(&book).map_err(|e| e.to_string())?;
            *slot = Some(Cached {
                dir: item.dir.clone(),
                stamp: now,
                model,
            });
        }
        let Some(cached) = slot.as_mut() else {
            return Err("sheet model unavailable".into());
        };
        let out = edit_and_report(&item, &mut cached.model, args)?;
        cached.stamp = stamp(&item);
        Ok(text(&out))
    }
}

/// Applies edits (structure, fill, set, style), saves once, then answers the read parts.
fn edit_and_report(
    item: &WorkItem,
    model: &mut SheetModel,
    args: &Value,
) -> Result<String, String> {
    let mut out = String::new();
    let done = sheet_ops::structure(model, args)?;
    let mut changed = sheet_ops::fill(model, args)?;
    if let Some(set) = args.get("set").and_then(Value::as_object) {
        let mut edits = Vec::new();
        for (key, value) in set {
            let (sheet, cell) = split(model, key);
            let (r, c) = parse_cell(&cell).ok_or_else(|| format!("bad cell {key}"))?;
            let idx = model
                .sheet_index(&sheet)
                .ok_or_else(|| format!("no sheet {sheet}"))?;
            edits.push((idx, r, c, value.clone()));
        }
        model.set_cells(&edits).map_err(|e| e.to_string())?;
        changed.extend(edits.into_iter().map(|(s, r, c, _)| (s, r, c)));
    }
    let styled = sheet_ops::style(model, args)?;
    if !done.is_empty() || !changed.is_empty() || styled > 0 {
        item.save(&Source::Book(model.book().clone()))
            .map_err(|e| e.to_string())?;
        out.push_str("Saved.");
        for d in &done {
            let _ = write!(out, " {d}.");
        }
        if styled > 0 {
            let _ = write!(out, " Styled {styled} range(s).");
        }
        out.push('\n');
    }
    if !changed.is_empty() {
        out.push_str("New values (computed):\n");
        for (idx, r, c) in changed.iter().take(40) {
            let name = model
                .book()
                .sheets
                .get(*idx as usize)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let shown = cell_text(model, *idx, *r, *c, "")
                .unwrap_or_else(|| format!("{}=(empty)", cell_name(*r, *c)));
            let _ = writeln!(out, "{name}!{shown}");
        }
        if changed.len() > 40 {
            let _ = writeln!(out, "… {} more cells.", changed.len() - 40);
        }
    }
    let needle = arg(args, "find").trim();
    if !needle.is_empty() {
        let exact = args.get("exact").and_then(Value::as_bool) == Some(true);
        out.push_str(&find(
            model,
            needle,
            exact,
            arg(args, "sheet").trim(),
            arg(args, "in").trim(),
        ));
    }
    if args.get("errors").and_then(Value::as_bool) == Some(true) {
        out.push_str(&sheet_ops::errors(model));
    }
    let trace = arg(args, "trace").trim();
    if !trace.is_empty() {
        out.push_str(&sheet_ops::trace(model, trace)?);
    }
    if args.get("diff").and_then(Value::as_bool) == Some(true) {
        out.push_str(&sheet_ops::diff(item, model));
    }
    let read_arg = arg(args, "read").trim();
    if read_arg == "*" || (read_arg.is_empty() && out.is_empty()) {
        out.push_str(&overview(model));
    } else if !read_arg.is_empty() {
        out.push_str(&read(model, read_arg, arg(args, "mode").trim())?);
    }
    Ok(out)
}
