//! Reading OOXML packages: parts by name, relationship maps and relative target resolution.

use std::collections::HashMap;
use std::io::Read as _;

use crate::error::{WorkError, WorkResult};

pub(crate) struct Zip {
    archive: zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
}

impl Zip {
    pub(crate) fn new(bytes: &[u8]) -> WorkResult<Self> {
        let archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec()))
            .map_err(|e| WorkError::Import(format!("not an Office file: {e}")))?;
        Ok(Self { archive })
    }

    pub(crate) fn bytes(&mut self, name: &str) -> Option<Vec<u8>> {
        let mut f = self.archive.by_name(name.trim_start_matches('/')).ok()?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    pub(crate) fn text(&mut self, name: &str) -> Option<String> {
        self.bytes(name).and_then(|b| String::from_utf8(b).ok())
    }

    /// `rId` → absolute part name for the rels of `part` (`ppt/slides/slide1.xml`).
    pub(crate) fn rels(&mut self, part: &str) -> HashMap<String, String> {
        let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
        let rels_name = if dir.is_empty() {
            format!("_rels/{file}.rels")
        } else {
            format!("{dir}/_rels/{file}.rels")
        };
        let Some(text) = self.text(&rels_name) else {
            return HashMap::new();
        };
        let Ok(doc) = roxmltree::Document::parse(&text) else {
            return HashMap::new();
        };
        doc.descendants()
            .filter(|n| n.has_tag_name("Relationship"))
            .filter(|n| n.attribute("TargetMode") != Some("External"))
            .filter_map(|n| {
                Some((
                    n.attribute("Id")?.to_string(),
                    resolve(dir, n.attribute("Target")?),
                ))
            })
            .collect()
    }
}

/// Joins a relative target onto a folder (`ppt/slides` + `../media/a.png` → `ppt/media/a.png`).
pub(crate) fn resolve(dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Text of all `t` elements under a node, paragraphs separated by newlines.
pub(crate) fn node_text(node: roxmltree::Node) -> String {
    let mut out = String::new();
    for p in node.descendants().filter(|n| n.has_tag_name("p")) {
        let line: String = p
            .descendants()
            .filter(|n| n.has_tag_name("t"))
            .filter_map(|n| n.text())
            .collect();
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_targets() {
        assert_eq!(
            resolve("ppt/slides", "../media/image1.png"),
            "ppt/media/image1.png"
        );
        assert_eq!(resolve("word", "media/a.png"), "word/media/a.png");
        assert_eq!(resolve("ppt", "/ppt/x.xml"), "ppt/x.xml");
    }
}
