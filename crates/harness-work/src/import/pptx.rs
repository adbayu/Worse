//! `.pptx` → `deck.json`. Each slide maps to the closest layout: title/subtitle placeholders,
//! bullet bodies, the first table, the first picture (copied to `assets/`) and speaker notes.
//! Exact positions, animations and master graphics are not carried over.

use super::xml::{node_text, Zip};
use super::Imported;
use crate::error::{WorkError, WorkResult};
use crate::item::{to_json, Kind};
use crate::model::deck::{Deck, Layout, Slide, TableData};

#[derive(Default)]
struct Parts {
    title: String,
    subtitle: String,
    centered_title: bool,
    bodies: Vec<Vec<String>>,
    table: Option<TableData>,
    image: Option<String>,
}

fn ph_type(sp: roxmltree::Node) -> Option<String> {
    sp.descendants()
        .find(|n| n.has_tag_name("ph"))
        .map(|n| n.attribute("type").unwrap_or("body").to_string())
}

fn paragraphs(sp: roxmltree::Node) -> Vec<String> {
    sp.descendants()
        .filter(|n| n.has_tag_name("p") && n.parent().is_some_and(|p| p.has_tag_name("txBody")))
        .map(|p| {
            let mut s = String::new();
            for r in p.children().filter(|n| n.has_tag_name("r")) {
                let t: String = r
                    .descendants()
                    .filter(|n| n.has_tag_name("t"))
                    .filter_map(|n| n.text())
                    .collect();
                let bold = r
                    .children()
                    .find(|n| n.has_tag_name("rPr"))
                    .and_then(|n| n.attribute("b"))
                    == Some("1");
                if bold && !t.trim().is_empty() {
                    use std::fmt::Write as _;
                    let _ = write!(s, "**{}**", t.trim());
                } else {
                    s.push_str(&t);
                }
            }
            s.trim().to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn table(frame: roxmltree::Node) -> Option<TableData> {
    let tbl = frame.descendants().find(|n| n.has_tag_name("tbl"))?;
    let mut rows: Vec<Vec<String>> = tbl
        .children()
        .filter(|n| n.has_tag_name("tr"))
        .map(|tr| {
            tr.children()
                .filter(|n| n.has_tag_name("tc"))
                .map(|tc| node_text(tc).replace('\n', " "))
                .collect()
        })
        .collect();
    if rows.is_empty() {
        return None;
    }
    let header = rows.remove(0);
    Some(TableData { header, rows })
}

fn read_slide(
    z: &mut Zip,
    part: &str,
    n: usize,
    assets: &mut Vec<(String, Vec<u8>)>,
) -> Option<Parts> {
    let xml = z.text(part)?;
    let doc = roxmltree::Document::parse(&xml).ok()?;
    let rels = z.rels(part);
    let mut p = Parts::default();
    for node in doc.descendants() {
        if node.has_tag_name("sp") {
            let paras = paragraphs(node);
            if paras.is_empty() {
                continue;
            }
            // 标题本身是粗体样式，去掉 ** 标记。
            let plain = || {
                paras
                    .iter()
                    .map(|t| t.replace("**", ""))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            match ph_type(node).as_deref() {
                Some("title") => p.title = plain(),
                Some("ctrTitle") => {
                    p.title = plain();
                    p.centered_title = true;
                }
                Some("subTitle") => p.subtitle = plain(),
                Some("sldNum" | "dt" | "ftr") => {}
                _ if p.title.is_empty() && paras.len() == 1 && p.bodies.is_empty() => {
                    p.title = paras.join(" ");
                }
                _ => p.bodies.push(paras),
            }
        } else if node.has_tag_name("graphicFrame") && p.table.is_none() {
            p.table = table(node);
        } else if node.has_tag_name("pic") && p.image.is_none() {
            let rid = node
                .descendants()
                .find(|n| n.has_tag_name("blip"))
                .and_then(|b| {
                    b.attributes()
                        .find(|a| a.name() == "embed")
                        .map(|a| a.value().to_string())
                });
            if let Some(target) = rid.and_then(|r| rels.get(&r).cloned()) {
                if let Some(bytes) = z.bytes(&target) {
                    let ext = target.rsplit('.').next().unwrap_or("png");
                    let name = format!("assets/slide{n}-image.{ext}");
                    assets.push((name.clone(), bytes));
                    p.image = Some(name);
                }
            }
        }
    }
    Some(p)
}

fn notes(z: &mut Zip, part: &str) -> String {
    let rels = z.rels(part);
    let Some(target) = rels.values().find(|t| t.contains("notesSlide")).cloned() else {
        return String::new();
    };
    let Some(xml) = z.text(&target) else {
        return String::new();
    };
    let Ok(doc) = roxmltree::Document::parse(&xml) else {
        return String::new();
    };
    doc.descendants()
        .filter(|n| n.has_tag_name("sp") && ph_type(*n).as_deref() == Some("body"))
        .map(node_text)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn to_slide(p: Parts, first: bool) -> Slide {
    let mut s = Slide {
        title: p.title,
        subtitle: p.subtitle,
        ..Slide::default()
    };
    let mut bodies = p.bodies;
    if p.centered_title || (first && bodies.is_empty() && p.table.is_none() && p.image.is_none()) {
        s.layout = Layout::Title;
        if s.subtitle.is_empty() {
            s.subtitle = bodies.into_iter().flatten().collect::<Vec<_>>().join(" ");
        }
        return s;
    }
    if let Some(t) = p.table {
        s.layout = Layout::Table;
        s.table = Some(t);
    } else if let Some(img) = p.image {
        s.image = Some(img);
        if bodies.is_empty() {
            s.layout = Layout::Image;
        } else {
            s.layout = Layout::ImageText;
            s.bullets = bodies.into_iter().flatten().collect();
        }
    } else if bodies.len() >= 2 {
        s.layout = Layout::TwoColumn;
        s.left = bodies.remove(0);
        s.right = bodies.into_iter().flatten().collect();
    } else if bodies.is_empty() {
        s.layout = Layout::Section;
    } else {
        s.layout = Layout::Bullets;
        s.bullets = bodies.into_iter().flatten().collect();
    }
    s
}

pub(crate) fn pptx(bytes: &[u8], stem: &str) -> WorkResult<Imported> {
    let mut z = Zip::new(bytes)?;
    let pres = z
        .text("ppt/presentation.xml")
        .ok_or_else(|| WorkError::Import("ppt/presentation.xml is missing".into()))?;
    let doc = roxmltree::Document::parse(&pres).map_err(|e| WorkError::Import(e.to_string()))?;
    let rels = z.rels("ppt/presentation.xml");
    let order: Vec<String> = doc
        .descendants()
        .filter(|n| n.has_tag_name("sldId"))
        .filter_map(|n| {
            n.attributes()
                .find(|a| a.name() == "id" && a.namespace().is_some())
                .map(|a| a.value().to_string())
        })
        .filter_map(|rid| rels.get(&rid).cloned())
        .collect();
    let mut deck = Deck {
        title: stem.to_string(),
        ..Deck::default()
    };
    let mut assets = Vec::new();
    let mut warnings = Vec::new();
    for (i, part) in order.iter().enumerate() {
        let Some(parts) = read_slide(&mut z, part, i + 1, &mut assets) else {
            warnings.push(format!("Slide {} could not be read.", i + 1));
            continue;
        };
        let mut slide = to_slide(parts, i == 0);
        slide.notes = notes(&mut z, part);
        deck.slides.push(slide);
    }
    if let Some(t) = deck
        .slides
        .first()
        .filter(|s| s.layout == Layout::Title && !s.title.is_empty())
    {
        deck.title.clone_from(&t.title);
    }
    warnings.push("Slides were mapped to Harness layouts; positions, animations and master graphics were not kept.".into());
    deck.fill_ids();
    Ok(Imported {
        kind: Kind::Deck,
        source: to_json(&deck)?,
        assets,
        warnings,
    })
}
