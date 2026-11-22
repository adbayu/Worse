//! `doc.md`: Markdown with a small frontmatter block. The body is parsed once into [`Block`]s that the
//! Typst, docx and html writers share.
//!
//! Extensions: `{#id}` after a heading, `<!-- pagebreak -->`, callouts `> [!NOTE]`, footnotes
//! `[^1]`, image captions `![caption](assets/x.png){#fig-x}` and ```` ```chart ```` fences holding a
//! chart object.

mod parse;

use parse::parse_blocks;

use super::chart::Chart;
use super::text::Run;

#[derive(Debug, Clone, PartialEq)]
pub struct DocMeta {
    pub title: String,
    /// `a4` or `letter`.
    pub paper: String,
    /// Margin in centimetres.
    pub margin: f64,
    pub header: String,
    pub footer: String,
    pub cover: bool,
    /// Number headings `1.`, `1.1.`.
    pub numbering: bool,
    pub toc: bool,
    pub font: String,
}

impl Default for DocMeta {
    fn default() -> Self {
        Self {
            title: String::new(),
            paper: "a4".into(),
            margin: 2.2,
            header: String::new(),
            footer: String::new(),
            cover: false,
            numbering: false,
            toc: false,
            font: "Segoe UI".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(Run),
    Link { text: String, url: String },
    Note(Vec<Inline>),
    Break,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        id: String,
        text: Vec<Inline>,
    },
    Para(Vec<Inline>),
    List {
        ordered: bool,
        items: Vec<(u8, Vec<Inline>)>,
    },
    Table {
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Image {
        src: String,
        caption: String,
        id: String,
    },
    Code {
        lang: String,
        text: String,
    },
    Chart(Box<Chart>),
    Callout {
        kind: String,
        text: Vec<Inline>,
    },
    PageBreak,
    Rule,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Doc {
    pub meta: DocMeta,
    pub body: String,
}

impl Doc {
    /// Splits the frontmatter (`---` lines with `key: value`) from the body.
    #[must_use]
    pub fn parse(source: &str) -> Self {
        let mut meta = DocMeta::default();
        let src = source.trim_start_matches('\u{feff}');
        let mut body = src;
        if let Some(rest) = src.strip_prefix("---") {
            if let Some(end) = rest.find("\n---") {
                for line in rest[..end].lines() {
                    if let Some((k, v)) = line.split_once(':') {
                        apply_meta(&mut meta, k.trim(), v.trim().trim_matches('"'));
                    }
                }
                body = rest[end + 4..].trim_start_matches(['\r', '\n']);
            }
        }
        Self {
            meta,
            body: body.to_string(),
        }
    }

    #[must_use]
    pub fn blocks(&self) -> Vec<Block> {
        parse_blocks(&self.body)
    }

    /// Title from the frontmatter, else the first heading.
    #[must_use]
    pub fn title(&self) -> String {
        if !self.meta.title.is_empty() {
            return self.meta.title.clone();
        }
        self.blocks()
            .into_iter()
            .find_map(|b| match b {
                Block::Heading { text, .. } => Some(inline_plain(&text)),
                _ => None,
            })
            .unwrap_or_default()
    }
}

fn apply_meta(meta: &mut DocMeta, key: &str, value: &str) {
    let yes = matches!(value, "true" | "yes" | "on");
    match key {
        "title" => meta.title = value.into(),
        "paper" | "page" => meta.paper = value.to_ascii_lowercase(),
        "margin" => {
            meta.margin = value
                .trim_end_matches("cm")
                .trim()
                .parse()
                .unwrap_or(meta.margin);
        }
        "header" => meta.header = value.into(),
        "footer" => meta.footer = value.into(),
        "cover" => meta.cover = yes,
        "numbering" => meta.numbering = yes,
        "toc" => meta.toc = yes,
        "font" => meta.font = value.into(),
        _ => {}
    }
}

#[must_use]
pub fn inline_plain(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for i in inlines {
        match i {
            Inline::Text(r) => s.push_str(&r.text),
            Inline::Link { text, .. } => s.push_str(text),
            Inline::Note(_) => {}
            Inline::Break => s.push(' '),
        }
    }
    s
}

/// Slug used for heading ids (`## Pricing` → `pricing`).
#[must_use]
pub fn heading_id(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests;
