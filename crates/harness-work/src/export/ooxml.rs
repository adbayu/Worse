//! Shared OOXML helpers: XML escaping, relationship parts, colors and a small zip package writer.

use std::io::Write as _;

use zip::write::SimpleFileOptions;

use crate::error::{WorkError, WorkResult};

pub(crate) const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
pub(crate) const XML_HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;

pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `#3b6cf6` → `3B6CF6` (fallback when invalid).
pub(crate) fn hex(color: &str, fallback: &str) -> String {
    let c = color.trim().trim_start_matches('#');
    let c = if c.len() == 3 {
        c.chars().flat_map(|ch| [ch, ch]).collect()
    } else {
        c.to_string()
    };
    if c.len() == 6 && c.chars().all(|ch| ch.is_ascii_hexdigit()) {
        c.to_ascii_uppercase()
    } else {
        fallback.to_string()
    }
}

/// A `_rels` part from `(id, type suffix, target)`.
pub(crate) fn rels(items: &[(String, &str, String)]) -> String {
    use std::fmt::Write as _;
    let mut s = format!(
        r#"{XML_HEAD}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#
    );
    for (id, kind, target) in items {
        let _ = write!(
            s,
            r#"<Relationship Id="{id}" Type="{REL}/{kind}" Target="{}"/>"#,
            esc(target)
        );
    }
    s + "</Relationships>"
}

/// Zip package with `[Content_Types].xml` written last from the parts added.
pub(crate) struct Package {
    zip: zip::ZipWriter<std::io::Cursor<Vec<u8>>>,
    overrides: Vec<(String, String)>,
    images: Vec<String>,
}

impl Package {
    pub(crate) fn new() -> Self {
        Self {
            zip: zip::ZipWriter::new(std::io::Cursor::new(Vec::new())),
            overrides: Vec::new(),
            images: Vec::new(),
        }
    }

    pub(crate) fn put(&mut self, name: &str, data: &[u8]) -> WorkResult<()> {
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        self.zip
            .start_file(name, opts)
            .map_err(|e| WorkError::Export(e.to_string()))?;
        self.zip
            .write_all(data)
            .map_err(|e| WorkError::Export(e.to_string()))
    }

    /// Adds a part with a content-type override.
    pub(crate) fn part(&mut self, name: &str, content_type: &str, data: &[u8]) -> WorkResult<()> {
        self.overrides
            .push((format!("/{name}"), content_type.to_string()));
        self.put(name, data)
    }

    /// Adds a media file; its extension becomes a default content type.
    pub(crate) fn media(&mut self, name: &str, data: &[u8]) -> WorkResult<()> {
        if let Some(ext) = name.rsplit('.').next().map(str::to_ascii_lowercase) {
            if !self.images.contains(&ext) {
                self.images.push(ext);
            }
        }
        self.put(name, data)
    }

    pub(crate) fn finish(mut self) -> WorkResult<Vec<u8>> {
        use std::fmt::Write as _;
        let mut ct = format!(
            r#"{XML_HEAD}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>"#
        );
        for ext in &self.images {
            let mime = match ext.as_str() {
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "svg" => "image/svg+xml",
                _ => "image/png",
            };
            let _ = write!(ct, r#"<Default Extension="{ext}" ContentType="{mime}"/>"#);
        }
        for (name, kind) in &self.overrides {
            let _ = write!(ct, r#"<Override PartName="{name}" ContentType="{kind}"/>"#);
        }
        ct.push_str("</Types>");
        self.put("[Content_Types].xml", ct.as_bytes())?;
        let cursor = self
            .zip
            .finish()
            .map_err(|e| WorkError::Export(e.to_string()))?;
        Ok(cursor.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_and_escaping() {
        assert_eq!(hex("#3b6cf6", "000000"), "3B6CF6");
        assert_eq!(hex("#abc", "000000"), "AABBCC");
        assert_eq!(hex("blue", "000000"), "000000");
        assert_eq!(esc("a<b&\"c\""), "a&lt;b&amp;&quot;c&quot;");
    }
}
