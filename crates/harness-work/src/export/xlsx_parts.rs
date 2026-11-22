//! Package-level edits for the write-back `.xlsx` export: which cells got a new look (and the
//! `cellXfs` entry each one now uses), the calc chain removal and the recalculation flag in
//! `xl/workbook.xml`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use ironcalc::base::types::{Alignment, Style};
use ironcalc::base::Model;

use super::xlsx_patch::{err, Changes};
use super::xlsx_style::Styles;
use crate::error::WorkResult;
use crate::import::xml::Zip;
use crate::model::book::{parse_range, Book, Sheet};
use crate::sheet::SheetModel;

/// Most cells one sheet's style comparison looks at.
const MAX_STYLE_CELLS: usize = 500_000;

/// Adds a style edit to `diffs` for every cell whose look changed between `orig` and `book`;
/// returns the `styles.xml` holding their new formats (`None` when no look changed).
pub(super) fn restyle(
    orig_bytes: &[u8],
    orig: &Book,
    book: &Book,
    model: &SheetModel,
    diffs: &mut HashMap<String, Changes>,
) -> WorkResult<Option<Styles>> {
    let looks: Vec<(usize, Vec<(i32, i32)>)> = orig
        .sheets
        .iter()
        .zip(&book.sheets)
        .map(|(old, new)| look_cells(old, new))
        .enumerate()
        .filter(|(_, cells)| !cells.is_empty())
        .collect();
    if looks.is_empty() {
        return Ok(None);
    }
    let before = SheetModel::from_book(orig)?;
    let wb = ironcalc::import::load_from_xlsx_bytes(orig_bytes, "orig", "en", "UTC")
        .map_err(|e| err(format!("{e:?}")))?;
    let real = Model::from_workbook(wb, "en").map_err(err)?;
    let xml = Zip::new(orig_bytes)?
        .text("xl/styles.xml")
        .ok_or_else(|| err("the original has no xl/styles.xml"))?;
    let mut styles = Styles::new(xml).map_err(err)?;
    let mut any = false;
    for (i, cells) in looks {
        let (Ok(idx), Some(sheet)) = (u32::try_from(i), book.sheets.get(i)) else {
            continue;
        };
        let rows = diffs.entry(sheet.name.clone()).or_default();
        for (r, c) in cells {
            let style = |m: &Model| m.get_style_for_cell(idx, r, c).unwrap_or_default();
            let (old, new) = (style(before.model()), style(model.model()));
            if old != new {
                let xf = styles.xf(&merge(&style(&real), &old, &new));
                rows.entry(r).or_default().entry(c).or_default().style = Some(xf);
                any = true;
            }
        }
    }
    diffs.retain(|_, rows| !rows.is_empty());
    Ok(any.then_some(styles))
}

fn diff_keys<T: PartialEq>(
    a: &BTreeMap<String, T>,
    b: &BTreeMap<String, T>,
    out: &mut BTreeSet<String>,
) {
    out.extend(
        a.iter()
            .filter(|(k, v)| b.get(*k) != Some(v))
            .map(|(k, _)| k.clone()),
    );
    out.extend(b.keys().filter(|k| !a.contains_key(*k)).cloned());
}

fn pick<T: PartialEq + Clone>(real: &T, before: &T, after: &T) -> T {
    if before == after { real } else { after }.clone()
}

/// The original cell style `real` with only the attributes changed between `before` and `after`
/// (both rebuilt from `book.json`, which keeps a simplified look) replaced, so fonts and theme
/// colors the user did not touch stay exactly as in the original file.
pub(super) fn merge(real: &Style, before: &Style, after: &Style) -> Style {
    let mut st = real.clone();
    st.num_fmt = pick(&real.num_fmt, &before.num_fmt, &after.num_fmt);
    st.fill = pick(&real.fill, &before.fill, &after.fill);
    st.quote_prefix = pick(
        &real.quote_prefix,
        &before.quote_prefix,
        &after.quote_prefix,
    );
    let (rf, bf, af) = (&real.font, &before.font, &after.font);
    st.font.b = pick(&rf.b, &bf.b, &af.b);
    st.font.i = pick(&rf.i, &bf.i, &af.i);
    st.font.u = pick(&rf.u, &bf.u, &af.u);
    st.font.strike = pick(&rf.strike, &bf.strike, &af.strike);
    st.font.sz = pick(&rf.sz, &bf.sz, &af.sz);
    st.font.color = pick(&rf.color, &bf.color, &af.color);
    let (rb, bb, ab) = (&real.border, &before.border, &after.border);
    st.border.left = pick(&rb.left, &bb.left, &ab.left);
    st.border.right = pick(&rb.right, &bb.right, &ab.right);
    st.border.top = pick(&rb.top, &bb.top, &ab.top);
    st.border.bottom = pick(&rb.bottom, &bb.bottom, &ab.bottom);
    let al = |s: &Style| s.alignment.clone().unwrap_or_default();
    let (ra, ba, aa) = (al(real), al(before), al(after));
    let align = Alignment {
        horizontal: pick(&ra.horizontal, &ba.horizontal, &aa.horizontal),
        vertical: pick(&ra.vertical, &ba.vertical, &aa.vertical),
        wrap_text: pick(&ra.wrap_text, &ba.wrap_text, &aa.wrap_text),
    };
    st.alignment = (align != Alignment::default()).then_some(align);
    st
}

/// Cells covered by a look entry (`formats`, `fills`, `bold`, …) that differs between the
/// original and the edited sheet.
pub(super) fn look_cells(old: &Sheet, new: &Sheet) -> Vec<(i32, i32)> {
    let mut keys = BTreeSet::new();
    for (a, b) in [
        (&old.formats, &new.formats),
        (&old.fills, &new.fills),
        (&old.colors, &new.colors),
        (&old.align, &new.align),
        (&old.borders, &new.borders),
        (&old.border_colors, &new.border_colors),
    ] {
        diff_keys(a, b, &mut keys);
    }
    diff_keys(&old.sizes, &new.sizes, &mut keys);
    for (a, b) in [
        (&old.bold, &new.bold),
        (&old.italic, &new.italic),
        (&old.underline, &new.underline),
        (&old.wrap, &new.wrap),
    ] {
        keys.extend(a.iter().filter(|k| !b.contains(k)).cloned());
        keys.extend(b.iter().filter(|k| !a.contains(k)).cloned());
    }
    let mut cells = BTreeSet::new();
    'ranges: for (r1, c1, r2, c2) in keys.iter().filter_map(|k| parse_range(k)) {
        for r in r1..=r2.min(r1 + 20_000) {
            for c in c1..=c2.min(c1 + 1_000) {
                cells.insert((r, c));
                if cells.len() >= MAX_STYLE_CELLS {
                    break 'ranges;
                }
            }
        }
    }
    cells.into_iter().collect()
}

pub(super) fn strip_calc_chain(xml: &str) -> String {
    let Some(pos) = xml.find("calcChain.xml") else {
        return xml.to_string();
    };
    let (Some(start), Some(end)) = (xml[..pos].rfind("<Override"), xml[pos..].find("/>")) else {
        return xml.to_string();
    };
    format!("{}{}", &xml[..start], &xml[pos + end + 2..])
}

/// 未修改的公式仍带旧缓存值，要求 Excel 打开时全部重算。
pub(super) fn full_calc_on_load(xml: &str) -> String {
    if xml.contains("fullCalcOnLoad") {
        return xml.to_string();
    }
    if let Some(pos) = xml.find("<calcPr") {
        let at = pos + "<calcPr".len();
        return format!(r#"{} fullCalcOnLoad="1"{}"#, &xml[..at], &xml[at..]);
    }
    // calcPr follows sheets / definedNames in the schema order.
    let anchor = ["</definedNames>", "</sheets>"]
        .iter()
        .find_map(|tag| xml.find(tag).map(|p| p + tag.len()));
    match anchor {
        Some(at) => format!(
            r#"{}<calcPr fullCalcOnLoad="1"/>{}"#,
            &xml[..at],
            &xml[at..]
        ),
        None => xml.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_recalc_on_open() {
        let wb = "<workbook><sheets><sheet/></sheets></workbook>";
        assert!(full_calc_on_load(wb).contains(r#"</sheets><calcPr fullCalcOnLoad="1"/>"#));
        let wb = r#"<workbook><calcPr calcId="191029"/></workbook>"#;
        assert!(full_calc_on_load(wb).contains(r#"<calcPr fullCalcOnLoad="1" calcId"#));
    }
}
