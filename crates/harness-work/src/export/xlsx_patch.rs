//! Write-back `.xlsx` export: takes the imported original `.xlsx` zip as a base and patches only
//! cells whose value or look changed in `book.json`, keeping charts, pivots, macros, drawings,
//! images, validations, conditional formatting and every untouched cell format.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Display;
use std::io::{Cursor, Read as _, Write as _};
use std::ops::Range;

use ironcalc::base::cell::CellValue;
use roxmltree::{Document, Node};
use serde_json::Value;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::ooxml::esc;
use super::xlsx_parts::{full_calc_on_load, restyle, strip_calc_chain};
use crate::error::{WorkError, WorkResult};
use crate::import::{xlsx_extra::sheet_parts, xml::Zip};
use crate::item::{Kind, Source, WorkItem};
use crate::model::book::{parse_cell, Book};
use crate::sheet::{cell_name, SheetModel};

/// What changed in one cell.
#[derive(Clone, Default)]
pub(super) struct Edit {
    /// New value (`Value::Null` = cleared); `None` keeps the original content.
    pub value: Option<Value>,
    /// New `cellXfs` index; `None` keeps the original `s`.
    pub style: Option<usize>,
}

/// row → column → edit.
pub(super) type Changes = BTreeMap<i32, BTreeMap<i32, Edit>>;

pub(super) fn err(e: impl Display) -> WorkError {
    WorkError::Invalid(format!("xlsx write-back failed: {e}"))
}

fn is(n: &Node, tag: &str) -> bool {
    n.is_element() && n.tag_name().name() == tag
}

/// Patches `orig_bytes` with every cell change between the original workbook and `book`.
pub(super) fn patch(orig_bytes: &[u8], book: &Book, model: &SheetModel) -> WorkResult<Vec<u8>> {
    let imported = crate::import::sheet::xlsx_plain(orig_bytes, "orig")?;
    let Source::Book(orig) = WorkItem::parse(Kind::Book, &imported.source).map_err(err)? else {
        return Err(err("the original file is not a workbook"));
    };
    let names = |b: &Book| b.sheets.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    if names(&orig) != names(book) {
        return Err(err("sheets were added, removed, renamed or reordered"));
    }
    let mut diffs = changes(&orig, book);
    let mut styles = restyle(orig_bytes, &orig, book, model, &mut diffs)?;
    if diffs.is_empty() {
        return Ok(orig_bytes.to_vec());
    }
    let parts: HashMap<String, String> = sheet_parts(&mut Zip::new(orig_bytes)?)
        .into_iter()
        .map(|(name, part)| (part, name))
        .collect();
    if let Some(missing) = diffs.keys().find(|s| !parts.values().any(|n| n == *s)) {
        return Err(err(format!("sheet {missing} has no worksheet part")));
    }

    let mut archive = ZipArchive::new(Cursor::new(orig_bytes)).map_err(err)?;
    let mut out = Cursor::new(Vec::with_capacity(orig_bytes.len()));
    let mut writer = ZipWriter::new(&mut out);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(err)?;
        let name = entry.name().map_err(err)?.into_owned();
        // 删除 calcChain.xml，让 Excel 重建计算顺序，避免“修复”提示。
        if name.eq_ignore_ascii_case("xl/calcChain.xml") {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).map_err(err)?;
        let data = match name.as_str() {
            "[Content_Types].xml" => strip_calc_chain(&String::from_utf8_lossy(&buf)).into_bytes(),
            "xl/workbook.xml" => full_calc_on_load(&String::from_utf8_lossy(&buf)).into_bytes(),
            "xl/styles.xml" => match styles
                .take()
                .map(super::xlsx_style::Styles::finish)
                .transpose()
                .map_err(err)?
            {
                Some(Some(xml)) => xml.into_bytes(),
                _ => buf,
            },
            _ => match parts.get(&name).and_then(|s| Some((s, diffs.get(s)?))) {
                Some((sheet, rows)) => {
                    let idx = model.sheet_index(sheet).unwrap_or(0);
                    let cells = book
                        .sheets
                        .iter()
                        .find(|s| &s.name == sheet)
                        .map(|s| &s.cells);
                    let ctx = Ctx { model, idx, cells };
                    patch_sheet(&String::from_utf8_lossy(&buf), rows, &ctx)?.into_bytes()
                }
                None => buf,
            },
        };
        writer.start_file(&name, opts).map_err(err)?;
        writer.write_all(&data).map_err(err)?;
    }
    // 单元格已引用新的样式索引，styles.xml 没写入就会得到损坏的文件。
    if styles.is_some() {
        return Err(err("xl/styles.xml was not found in the package"));
    }
    writer.finish().map_err(err)?;
    Ok(out.into_inner())
}

/// Changed and cleared cells per sheet name.
fn changes(orig: &Book, book: &Book) -> HashMap<String, Changes> {
    let mut diffs = HashMap::new();
    for (old, sh) in orig.sheets.iter().zip(&book.sheets) {
        let mut rows = Changes::new();
        let mut put = |coord: &str, v: Option<&Value>| {
            if let Some((r, c)) = parse_cell(coord) {
                let edit = rows.entry(r).or_default().entry(c).or_default();
                edit.value = Some(v.cloned().unwrap_or(Value::Null));
            }
        };
        for (coord, val) in &sh.cells {
            if old.cells.get(coord) != Some(val) {
                put(coord, Some(val));
            }
        }
        for coord in old.cells.keys().filter(|k| !sh.cells.contains_key(*k)) {
            put(coord, None);
        }
        if !rows.is_empty() {
            diffs.insert(sh.name.clone(), rows);
        }
    }
    diffs
}

struct Ctx<'a> {
    model: &'a SheetModel,
    idx: u32,
    cells: Option<&'a BTreeMap<String, Value>>,
}

impl Ctx<'_> {
    /// The cell after `edit`; `orig` is the original `<c>` text and its `s`, when there is one.
    fn apply(&self, row: i32, col: i32, orig: Option<(&str, Option<&str>)>, edit: &Edit) -> String {
        let xf = edit.style.map(|s| s.to_string());
        match (&edit.value, orig) {
            (Some(val), orig) => {
                let style = xf.as_deref().or(orig.and_then(|(_, s)| s));
                self.cell(row, col, style, Some(val))
            }
            (None, Some((src, _))) => xf.map_or_else(|| src.to_string(), |s| set_style(src, &s)),
            (None, None) => xf.map_or_else(String::new, |s| {
                format!(r#"<c r="{}" s="{s}"/>"#, cell_name(row, col))
            }),
        }
    }

    /// One `<c>` element with style index `style`. Empty when nothing is left.
    fn cell(&self, row: i32, col: i32, style: Option<&str>, val: Option<&Value>) -> String {
        let coord = cell_name(row, col);
        let style_attr = style.map_or_else(String::new, |s| format!(r#" s="{s}""#));
        let (kind, body) = match val {
            None | Some(Value::Null) if style.is_some() => {
                return format!(r#"<c r="{coord}"{style_attr}/>"#)
            }
            None | Some(Value::Null) => return String::new(),
            Some(Value::Bool(flag)) => (r#" t="b""#, format!("<v>{}</v>", u8::from(*flag))),
            Some(Value::Number(num)) => ("", format!("<v>{num}</v>")),
            Some(Value::String(formula)) if formula.starts_with('=') => {
                let (kind, cached) = match self.model.value(self.idx, row, col) {
                    CellValue::Number(num) => ("", format!("<v>{num}</v>")),
                    CellValue::Boolean(flag) => (r#" t="b""#, format!("<v>{}</v>", u8::from(flag))),
                    CellValue::String(text) if !text.starts_with('#') => {
                        (r#" t="str""#, format!("<v>{}</v>", esc(&text)))
                    }
                    _ => ("", String::new()),
                };
                (kind, format!("<f>{}</f>{cached}", esc(&formula[1..])))
            }
            Some(other) => {
                let text = other
                    .as_str()
                    .map_or_else(|| other.to_string(), str::to_string);
                let inline = format!(r#"<is><t xml:space="preserve">{}</t></is>"#, esc(&text));
                (r#" t="inlineStr""#, inline)
            }
        };
        format!(r#"<c r="{coord}"{style_attr}{kind}>{body}</c>"#)
    }
}

/// The original `<c>` text with its `s` attribute set to `s`; content stays byte-for-byte.
fn set_style(src: &str, s: &str) -> String {
    let head = &src[..src.find('>').unwrap_or(src.len())];
    if let Some(start) = head.find(" s=\"").map(|i| i + 4) {
        if let Some(len) = head[start..].find('"') {
            return format!("{}{s}{}", &src[..start], &src[start + len..]);
        }
    }
    let at = src
        .find(|ch: char| ch.is_whitespace() || ch == '>' || ch == '/')
        .unwrap_or(src.len());
    format!(r#"{} s="{s}"{}"#, &src[..at], &src[at..])
}

fn row_num(n: &Node) -> Option<i32> {
    n.attribute("r")?.parse().ok()
}

fn col_num(n: &Node) -> Option<i32> {
    parse_cell(n.attribute("r")?).map(|(_, c)| c)
}

/// Rewrites changed rows of `<sheetData>` keeping cells in column order and rows in row order;
/// everything outside the touched rows stays byte-for-byte identical.
fn patch_sheet(xml: &str, rows: &Changes, ctx: &Ctx) -> WorkResult<String> {
    let doc = Document::parse(xml).map_err(err)?;
    let sd = doc
        .descendants()
        .find(|n| is(n, "sheetData"))
        .ok_or_else(|| err("worksheet without sheetData"))?;
    let mut rows = rows.clone();
    unshare_formulas(sd, &mut rows, ctx);
    let existing: Vec<Node> = sd.children().filter(|n| is(n, "row")).collect();
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for row in &existing {
        if let Some(new) = row_num(row).and_then(|r| Some((r, rows.remove(&r)?))) {
            edits.push((row.range(), rebuild_row(xml, row, new.0, new.1, ctx)));
        }
    }
    let mut tail = String::new();
    for (r, cols) in rows {
        let body: String = cols
            .iter()
            .map(|(c, e)| ctx.apply(r, *c, None, e))
            .collect();
        if body.is_empty() {
            continue;
        }
        let text = format!(r#"<row r="{r}">{body}</row>"#);
        match existing.iter().find(|n| row_num(n).is_some_and(|x| x > r)) {
            Some(next) => edits.push((next.range().start..next.range().start, text)),
            None => tail.push_str(&text),
        }
    }
    if !tail.is_empty() {
        let range = sd.range();
        match xml[range.clone()].rfind("</") {
            Some(close) => edits.push((range.start + close..range.start + close, tail)),
            None => edits.push((range, format!("<sheetData>{tail}</sheetData>"))),
        }
    }
    edits.sort_by_key(|(r, _)| (r.start, r.end));
    let mut out = String::with_capacity(xml.len() + 256);
    let mut cursor = 0;
    for (range, text) in edits {
        out.push_str(&xml[cursor..range.start]);
        out.push_str(&text);
        cursor = range.end;
    }
    out.push_str(&xml[cursor..]);
    Ok(out)
}

fn shared<'a, 'i>(c: &Node<'a, 'i>) -> Option<Node<'a, 'i>> {
    c.children()
        .find(|f| is(f, "f") && f.attribute("t") == Some("shared"))
}

/// 共享公式的主单元格被改时，同组其它单元格改写成普通公式，否则它们会失去公式。
fn unshare_formulas(sd: Node, rows: &mut Changes, ctx: &Ctx) {
    let changed = |c: &Node, rows: &Changes| {
        let (Some(r), Some(col)) = (c.parent().as_ref().and_then(row_num), col_num(c)) else {
            return false;
        };
        let edit = rows.get(&r).and_then(|m| m.get(&col));
        edit.is_some_and(|e| e.value.is_some())
    };
    let cells: Vec<Node> = sd.descendants().filter(|n| is(n, "c")).collect();
    let broken: HashSet<&str> = cells
        .iter()
        .filter(|c| changed(c, rows))
        .filter_map(|c| {
            shared(c)
                .filter(|f| f.has_attribute("ref"))?
                .attribute("si")
        })
        .collect();
    for c in &cells {
        let in_group = shared(c)
            .and_then(|f| f.attribute("si"))
            .is_some_and(|si| broken.contains(si));
        if !in_group || changed(c, rows) {
            continue;
        }
        if let (Some(coord), Some(cells)) = (c.attribute("r"), ctx.cells) {
            if let Some((r, col)) = parse_cell(coord) {
                let edit = rows.entry(r).or_default().entry(col).or_default();
                edit.value = Some(cells.get(coord).cloned().unwrap_or(Value::Null));
            }
        }
    }
}

/// The row element with the changed cells merged in column order.
fn rebuild_row(xml: &str, row: &Node, r: i32, mut new: BTreeMap<i32, Edit>, ctx: &Ctx) -> String {
    let src = &xml[row.range()];
    let Some(gt) = src.find('>') else {
        return src.to_string();
    };
    // `<row r="3"/>` 自闭合时没有 `</row>`，需要展开。
    let open = src[..gt].strip_suffix('/').unwrap_or(&src[..gt]);
    let (mut body, mut trailing) = (String::new(), String::new());
    for child in row.children().filter(Node::is_element) {
        let Some(col) = is(&child, "c").then(|| col_num(&child)).flatten() else {
            if is(&child, "c") {
                body.push_str(&xml[child.range()]);
            } else {
                trailing.push_str(&xml[child.range()]);
            }
            continue;
        };
        while let Some((c, e)) = new.pop_first() {
            if c >= col {
                new.insert(c, e);
                break;
            }
            body.push_str(&ctx.apply(r, c, None, &e));
        }
        let orig = (&xml[child.range()], child.attribute("s"));
        match new.remove(&col) {
            Some(e) => body.push_str(&ctx.apply(r, col, Some(orig), &e)),
            None => body.push_str(orig.0),
        }
    }
    for (c, e) in new {
        body.push_str(&ctx.apply(r, c, None, &e));
    }
    format!("{open}>{body}{trailing}</row>")
}

#[cfg(test)]
mod tests;
