//! `.docx` → `doc.md`: headings (by style or outline level), bold/italic runs, hyperlinks, lists,
//! tables and images (copied to `assets/`). Tracked changes, comments and text boxes are dropped.

use std::collections::HashMap;

use super::xml::Zip;
use super::Imported;
use crate::error::{WorkError, WorkResult};
use crate::item::Kind;

fn attr<'a>(n: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}

fn child<'a>(n: roxmltree::Node<'a, 'a>, tag: &str) -> Option<roxmltree::Node<'a, 'a>> {
    n.children().find(|c| c.has_tag_name(tag))
}

/// Escapes characters Markdown would treat as syntax at the start or inside text.
fn md_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('`', "\\`")
        .replace('|', "\\|")
}

struct Ctx<'a> {
    rels: &'a HashMap<String, String>,
    links: HashMap<String, String>,
    assets: Vec<(String, Vec<u8>)>,
    zip: &'a mut Zip,
}

impl Ctx<'_> {
    fn run(&mut self, r: roxmltree::Node) -> String {
        let props = child(r, "rPr");
        let on = |tag: &str| {
            props
                .and_then(|p| child(p, tag))
                .is_some_and(|n| !matches!(attr(n, "val"), Some("0" | "false")))
        };
        let mut text = String::new();
        for c in r.children() {
            if c.has_tag_name("t") {
                text.push_str(&md_escape(c.text().unwrap_or_default()));
            } else if c.has_tag_name("tab") {
                text.push(' ');
            } else if c.has_tag_name("br") {
                text.push_str("  \n");
            } else if c.has_tag_name("drawing") {
                text.push_str(&self.image(c));
            }
        }
        if text.trim().is_empty() {
            return text;
        }
        let (lead, core, trail) = split_ws(&text);
        let mut core = core.to_string();
        if on("i") {
            core = format!("*{core}*");
        }
        if on("b") {
            core = format!("**{core}**");
        }
        format!("{lead}{core}{trail}")
    }

    fn image(&mut self, drawing: roxmltree::Node) -> String {
        let Some(rid) = drawing
            .descendants()
            .find(|n| n.has_tag_name("blip"))
            .and_then(|b| attr(b, "embed"))
        else {
            return String::new();
        };
        let Some(target) = self.rels.get(rid).cloned() else {
            return String::new();
        };
        let Some(bytes) = self.zip.bytes(&target) else {
            return String::new();
        };
        let ext = target.rsplit('.').next().unwrap_or("png");
        let name = format!("assets/image{}.{ext}", self.assets.len() + 1);
        self.assets.push((name.clone(), bytes));
        format!("![]({name})")
    }

    fn para(&mut self, p: roxmltree::Node) -> String {
        let mut out = String::new();
        for c in p.children() {
            if c.has_tag_name("r") {
                out.push_str(&self.run(c));
            } else if c.has_tag_name("hyperlink") {
                let inner: String = c
                    .children()
                    .filter(|n| n.has_tag_name("r"))
                    .map(|r| self.run(r))
                    .collect();
                match attr(c, "id").and_then(|id| self.links.get(id)) {
                    Some(url) => {
                        use std::fmt::Write as _;
                        let _ = write!(out, "[{inner}]({url})");
                    }
                    None => out.push_str(&inner),
                }
            } else if c.has_tag_name("ins") || c.has_tag_name("smartTag") {
                let inner: String = c
                    .children()
                    .filter(|n| n.has_tag_name("r"))
                    .map(|r| self.run(r))
                    .collect();
                out.push_str(&inner);
            }
        }
        out.trim().to_string()
    }

    fn table(&mut self, t: roxmltree::Node) -> String {
        let rows: Vec<Vec<String>> = t
            .children()
            .filter(|n| n.has_tag_name("tr"))
            .map(|tr| {
                tr.children()
                    .filter(|n| n.has_tag_name("tc"))
                    .map(|tc| {
                        tc.children()
                            .filter(|n| n.has_tag_name("p"))
                            .map(|p| self.para(p))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect()
            })
            .collect();
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return String::new();
        }
        let line = |r: &Vec<String>| {
            format!(
                "| {} |",
                (0..cols)
                    .map(|c| r.get(c).map_or("", String::as_str).replace('\n', " "))
                    .collect::<Vec<_>>()
                    .join(" | ")
            )
        };
        // 表头单元格本身就是粗体，去掉多余的 ** 标记。
        let head: Vec<String> = rows[0]
            .iter()
            .map(|c| {
                c.trim_start_matches("**")
                    .trim_end_matches("**")
                    .to_string()
            })
            .collect();
        let mut out = vec![line(&head), format!("|{}|", vec!["---"; cols].join("|"))];
        out.extend(rows.iter().skip(1).map(line));
        out.join("\n")
    }
}

fn split_ws(s: &str) -> (&str, &str, &str) {
    let start = s.len() - s.trim_start().len();
    let end = s.trim_end().len();
    (&s[..start], &s[start..end], &s[end..])
}

/// Heading level from the paragraph style (`Heading2`, `Title`) or outline level.
fn heading(p: roxmltree::Node) -> Option<usize> {
    let props = child(p, "pPr")?;
    if let Some(lvl) = child(props, "outlineLvl")
        .and_then(|n| attr(n, "val"))
        .and_then(|v| v.parse::<usize>().ok())
    {
        return Some((lvl + 1).min(6));
    }
    let style = child(props, "pStyle")
        .and_then(|n| attr(n, "val"))?
        .to_ascii_lowercase();
    if style == "title" {
        return Some(1);
    }
    style
        .strip_prefix("heading")
        .and_then(|n| n.trim().parse::<usize>().ok())
        .map(|n| n.clamp(1, 6))
}

fn list_level(p: roxmltree::Node) -> Option<usize> {
    let num = child(child(p, "pPr")?, "numPr")?;
    Some(
        child(num, "ilvl")
            .and_then(|n| attr(n, "val"))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    )
}

/// Lists written with a typed marker and a hanging indent (`1.\t`, `•\t`), as Harness exports them.
fn manual_marker<'t>(p: roxmltree::Node, text: &'t str) -> Option<(usize, bool, &'t str)> {
    let (marker, rest) = text.split_once('\t')?;
    let ordered = marker
        .strip_suffix(['.', ')'])
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if !ordered && !matches!(marker, "•" | "◦" | "▪" | "-" | "–") {
        return None;
    }
    let ind = child(child(p, "pPr")?, "ind");
    let left: i64 = ind
        .and_then(|n| attr(n, "left").or_else(|| attr(n, "start")))
        .and_then(|v| v.parse().ok())
        .unwrap_or(360);
    let depth = usize::try_from((left - 360).max(0) / 360).unwrap_or(0);
    Some((depth, ordered, rest.trim_start()))
}

pub(crate) fn docx(bytes: &[u8], stem: &str) -> WorkResult<Imported> {
    let mut zip = Zip::new(bytes)?;
    let xml = zip
        .text("word/document.xml")
        .ok_or_else(|| WorkError::Import("word/document.xml is missing".into()))?;
    let doc = roxmltree::Document::parse(&xml).map_err(|e| WorkError::Import(e.to_string()))?;
    let rels = zip.rels("word/document.xml");
    let links = external_links(&mut zip);
    let body = doc
        .descendants()
        .find(|n| n.has_tag_name("body"))
        .ok_or_else(|| WorkError::Import("document has no body".into()))?;
    let mut cx = Ctx {
        rels: &rels,
        links,
        assets: Vec::new(),
        zip: &mut zip,
    };
    let mut blocks: Vec<String> = Vec::new();
    let mut title = String::new();
    for node in body.children() {
        if node.has_tag_name("p") {
            let text = cx.para(node);
            if text.is_empty() {
                continue;
            }
            if let Some(level) = heading(node) {
                if title.is_empty() {
                    title = text.replace(['*', '\\'], "");
                }
                blocks.push(format!("{} {text}", "#".repeat(level)));
            } else if let Some((depth, ordered, rest)) = list_level(node)
                .map(|d| (d, false, text.as_str()))
                .or_else(|| manual_marker(node, &text))
            {
                let bullet = if ordered { "1." } else { "-" };
                let item = format!("{}{bullet} {rest}", "  ".repeat(depth));
                match blocks
                    .last_mut()
                    .filter(|b| b.trim_start().starts_with(bullet))
                {
                    Some(last) => {
                        last.push('\n');
                        last.push_str(&item);
                    }
                    None => blocks.push(item),
                }
            } else {
                blocks.push(text);
            }
        } else if node.has_tag_name("tbl") {
            let t = cx.table(node);
            if !t.is_empty() {
                blocks.push(t);
            }
        }
    }
    let assets = std::mem::take(&mut cx.assets);
    if title.is_empty() {
        title = stem.to_string();
    }
    let source = format!(
        "---\ntitle: {}\n---\n{}\n",
        title.replace('\n', " "),
        blocks.join("\n\n")
    );
    let mut warnings = Vec::new();
    if xml.contains("<w:del ") || xml.contains("<w:ins ") {
        warnings.push("Tracked changes were accepted as shown.".into());
    }
    if zip.text("word/comments.xml").is_some() {
        warnings.push("Word comments were not imported.".into());
    }
    Ok(Imported {
        kind: Kind::Doc,
        source,
        assets,
        warnings,
    })
}

fn external_links(zip: &mut Zip) -> HashMap<String, String> {
    let Some(text) = zip.text("word/_rels/document.xml.rels") else {
        return HashMap::new();
    };
    let Ok(doc) = roxmltree::Document::parse(&text) else {
        return HashMap::new();
    };
    doc.descendants()
        .filter(|n| n.has_tag_name("Relationship") && n.attribute("TargetMode") == Some("External"))
        .filter_map(|n| {
            Some((
                n.attribute("Id")?.to_string(),
                n.attribute("Target")?.to_string(),
            ))
        })
        .collect()
}
