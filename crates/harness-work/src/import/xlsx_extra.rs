//! What `IronCalc` does not import, read straight from the .xlsx XML: dropdown lists (data
//! validation), conditional formatting cell rules, hyperlinks, the number-format locale and
//! workbook calc flags.

use std::collections::HashMap;

use roxmltree::{Document, Node};

use super::xml::{resolve, Zip};
use crate::model::book::{Book, Conditional};

/// Workbook-level facts worth telling the user about.
#[derive(Debug, Default)]
pub(super) struct Flags {
    pub date1904: bool,
    pub iterate: bool,
    pub external_links: usize,
    /// Language of the `[$-421]`-style locale tags in the number formats, when not English.
    pub locale: Option<&'static str>,
}

fn local<'a, 'i: 'a>(n: Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    n.descendants().filter(move |c| c.tag_name().name() == name)
}

fn truthy(v: Option<&str>) -> bool {
    matches!(v, Some("1" | "true"))
}

/// `r:id` of a `sheet` / `hyperlink` element (namespace prefix varies).
fn rel_id<'a>(n: Node<'a, '_>) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == "id").map(|a| a.value())
}

/// `rId` → target, including external targets (hyperlinks).
fn all_rels(zip: &mut Zip, part: &str) -> HashMap<String, String> {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    let Some(text) = zip.text(&format!("{dir}/_rels/{file}.rels")) else {
        return HashMap::new();
    };
    let Ok(doc) = Document::parse(&text) else {
        return HashMap::new();
    };
    local(doc.root(), "Relationship")
        .filter_map(|n| {
            let target = n.attribute("Target")?;
            let t = if n.attribute("TargetMode") == Some("External") {
                target.to_string()
            } else {
                resolve(dir, target)
            };
            Some((n.attribute("Id")?.to_string(), t))
        })
        .collect()
}

fn parts_of(zip: &mut Zip, wb: &Document) -> Vec<(String, String)> {
    let rels = zip.rels("xl/workbook.xml");
    local(wb.root(), "sheet")
        .filter_map(|n| {
            Some((
                n.attribute("name")?.to_string(),
                rels.get(rel_id(n)?)?.clone(),
            ))
        })
        .collect()
}

/// `(sheet name, worksheet part)` in workbook order, e.g. `("Sheet1", "xl/worksheets/sheet1.xml")`.
pub(crate) fn sheet_parts(zip: &mut Zip) -> Vec<(String, String)> {
    let Some(text) = zip.text("xl/workbook.xml") else {
        return Vec::new();
    };
    Document::parse(&text).map_or_else(|_| Vec::new(), |wb| parts_of(zip, &wb))
}

/// Fill colors of the differential formats (`dxfs`) used by conditional formatting.
fn dxf_fills(styles: &Document) -> Vec<Option<String>> {
    let Some(dxfs) = local(styles.root(), "dxfs").next() else {
        return Vec::new();
    };
    dxfs.children()
        .filter(|n| n.tag_name().name() == "dxf")
        .map(|dxf| {
            local(dxf, "bgColor")
                .chain(local(dxf, "fgColor"))
                .find_map(|c| c.attribute("rgb"))
                .map(|argb| format!("#{}", &argb[argb.len().saturating_sub(6)..]))
        })
        .collect()
}

/// Most used non-English locale among `[$Rp-421]` / `[$-407]` tags in the number formats.
fn format_locale(styles: &Document) -> Option<&'static str> {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for code in local(styles.root(), "numFmt").filter_map(|n| n.attribute("formatCode")) {
        for tag in code.split("[$").skip(1) {
            let lcid = tag
                .split_once(']')
                .and_then(|(t, _)| t.rsplit_once('-'))
                .and_then(|(_, hex)| u32::from_str_radix(hex, 16).ok());
            if let Some(lang) = lcid.and_then(crate::sheet::locale::from_lcid) {
                *counts.entry(lang).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .filter(|(lang, _)| *lang != "en")
        .max_by_key(|(lang, n)| (*n, std::cmp::Reverse(*lang)))
        .map(|(lang, _)| lang)
}

fn operator(op: &str) -> Option<&'static str> {
    Some(match op {
        "greaterThan" => ">",
        "lessThan" => "<",
        "equal" => "=",
        "notEqual" => "<>",
        "greaterThanOrEqual" => ">=",
        "lessThanOrEqual" => "<=",
        _ => return None,
    })
}

/// Reads validation lists, conditional rules and hyperlinks into `book`; returns the flags.
pub(super) fn read(bytes: &[u8], book: &mut Book) -> Flags {
    let mut flags = Flags::default();
    let Ok(mut zip) = Zip::new(bytes) else {
        return flags;
    };
    flags.external_links = (1..=64)
        .take_while(|i| {
            zip.bytes(&format!("xl/externalLinks/externalLink{i}.xml"))
                .is_some()
        })
        .count();
    let Some(wb_text) = zip.text("xl/workbook.xml") else {
        return flags;
    };
    let Ok(wb) = Document::parse(&wb_text) else {
        return flags;
    };
    flags.date1904 = local(wb.root(), "workbookPr").any(|n| truthy(n.attribute("date1904")));
    flags.iterate = local(wb.root(), "calcPr").any(|n| truthy(n.attribute("iterate")));
    let styles_text = zip.text("xl/styles.xml").unwrap_or_default();
    let styles = Document::parse(&styles_text).ok();
    let dxfs = styles.as_ref().map(dxf_fills).unwrap_or_default();
    flags.locale = styles.as_ref().and_then(format_locale);
    for (name, part) in parts_of(&mut zip, &wb) {
        let Some(sheet) = book.sheets.iter_mut().find(|s| s.name == name) else {
            continue;
        };
        let Some(text) = zip.text(&part) else {
            continue;
        };
        let Ok(doc) = Document::parse(&text) else {
            continue;
        };
        let links = all_rels(&mut zip, &part);
        for dv in local(doc.root(), "dataValidation") {
            if dv.attribute("type") != Some("list") {
                continue;
            }
            let Some(f) = local(dv, "formula1").next().and_then(|n| n.text()) else {
                continue;
            };
            let items: Vec<String> = match f.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                Some(list) => list.split(',').map(|s| s.trim().to_string()).collect(),
                None => vec![format!("={f}")],
            };
            for range in dv.attribute("sqref").unwrap_or_default().split_whitespace() {
                sheet.validation.insert(range.to_string(), items.clone());
            }
        }
        for cf in local(doc.root(), "conditionalFormatting") {
            let ranges = cf.attribute("sqref").unwrap_or_default().to_string();
            for rule in local(cf, "cfRule") {
                let (Some("cellIs"), Some(op)) = (
                    rule.attribute("type"),
                    rule.attribute("operator").and_then(operator),
                ) else {
                    continue;
                };
                let Some(fill) = rule
                    .attribute("dxfId")
                    .and_then(|d| d.parse::<usize>().ok())
                    .and_then(|d| dxfs.get(d).cloned().flatten())
                else {
                    continue;
                };
                let Some(value) = local(rule, "formula").next().and_then(|n| n.text()) else {
                    continue;
                };
                let value = value.trim_matches('"');
                for range in ranges.split_whitespace() {
                    sheet.conditional.push(Conditional {
                        range: range.to_string(),
                        when: format!("{op}{value}"),
                        fill: fill.clone(),
                    });
                }
            }
        }
        for h in local(doc.root(), "hyperlink") {
            let Some(cell) = h.attribute("ref") else {
                continue;
            };
            let target = rel_id(h)
                .and_then(|r| links.get(r).cloned())
                .or_else(|| h.attribute("location").map(|l| format!("#{l}")));
            if let Some(t) = target {
                sheet.links.insert(cell.to_string(), t);
            }
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_number_format_locale() {
        let xml = r#"<styleSheet><numFmts>
            <numFmt numFmtId="164" formatCode="[$Rp-421]#,##0.00"/>
            <numFmt numFmtId="165" formatCode="[$-421]dd/mm/yyyy"/>
            <numFmt numFmtId="166" formatCode="[$$-409]#,##0"/>
        </numFmts></styleSheet>"#;
        let doc = Document::parse(xml).unwrap();
        assert_eq!(format_locale(&doc), Some("id"));
    }
}
