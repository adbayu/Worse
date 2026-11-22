//! Validation with element paths: schema-level errors (wrong format, missing fields) and layout
//! warnings (text likely to overflow, a missing image, an empty slide, a bad cell reference).

use serde::Serialize;

use crate::item::{Kind, Source, WorkItem};
use crate::model::book::{parse_cell, parse_range, Book};
use crate::model::chart::{Chart, ChartType};
use crate::model::deck::{Deck, Layout, Slide};
use crate::model::doc::{Block, Doc};
use crate::model::{book, chart, deck};
use crate::path::ElementPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub severity: Severity,
    pub path: String,
    pub message: String,
}

impl Issue {
    fn error(path: &ElementPath, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            path: path.to_string(),
            message: message.into(),
        }
    }
    fn warn(path: &ElementPath, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.to_string(),
            message: message.into(),
        }
    }
}

/// Validates an item; a parse failure is a single error issue.
#[must_use]
pub fn validate(item: &WorkItem) -> Vec<Issue> {
    let file = item.kind.file_name();
    let text = match item.read_raw() {
        Ok(t) => t,
        Err(e) => return vec![Issue::error(&ElementPath::new(file, ""), e.to_string())],
    };
    match WorkItem::parse(item.kind, &text) {
        Ok(source) => validate_source(item, &source),
        Err(message) => vec![Issue::error(&ElementPath::new(file, ""), message)],
    }
}

#[must_use]
pub fn validate_source(item: &WorkItem, source: &Source) -> Vec<Issue> {
    let mut out = Vec::new();
    match source {
        Source::Deck(d) => check_deck(item, d, &mut out),
        Source::Book(b) => check_book(b, &mut out),
        Source::Doc(d) => check_doc(item, d, &mut out),
        Source::Chart(c) => {
            check_format(
                c.format.as_deref().unwrap_or(chart::FORMAT),
                chart::FORMAT,
                Kind::Chart,
                &mut out,
            );
            check_chart(c, &ElementPath::new("chart.json", ""), &mut out);
        }
    }
    out
}

fn check_format(found: &str, want: &str, kind: Kind, out: &mut Vec<Issue>) {
    if found != want {
        out.push(Issue::error(
            &ElementPath::new(kind.file_name(), "/format"),
            format!("format must be \"{want}\" (found \"{found}\")"),
        ));
    }
}

/// Rough capacity in characters of a text box at a font size (Segoe-like average glyph width).
fn capacity(width: f64, height: f64, size: f64) -> usize {
    let per_line = (width / (size * 0.52)).max(1.0);
    let lines = (height / (size * 1.3)).max(1.0);
    // 安全转换：容量最多几万字符，截断无害。
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cap = (per_line * lines) as usize;
    cap
}

fn check_deck(item: &WorkItem, d: &Deck, out: &mut Vec<Issue>) {
    check_format(&d.format, deck::FORMAT, Kind::Deck, out);
    if d.slides.is_empty() {
        out.push(Issue::warn(
            &ElementPath::new("deck.json", "/slides"),
            "the deck has no slides",
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for (i, s) in d.slides.iter().enumerate() {
        let at = |f: &str| ElementPath::slide(i, f);
        if !ids.insert(s.id.clone()) {
            out.push(Issue::error(
                &at("id"),
                format!("duplicate slide id \"{}\"", s.id),
            ));
        }
        check_slide(item, s, i, out);
        if s.title.chars().count() > 70 {
            out.push(Issue::warn(
                &at("title"),
                "title is long and may wrap to three lines; keep it under 70 characters",
            ));
        }
    }
}

fn check_slide(item: &WorkItem, s: &Slide, i: usize, out: &mut Vec<Issue>) {
    let at = |f: &str| ElementPath::slide(i, f);
    let need = |ok: bool, field: &str, out: &mut Vec<Issue>| {
        if !ok {
            out.push(Issue::error(
                &at(field),
                format!("layout \"{}\" needs \"{field}\"", s.layout.name()),
            ));
        }
    };
    match s.layout {
        Layout::Title | Layout::Section => need(!s.title.is_empty(), "title", out),
        Layout::Bullets => {
            need(!s.bullets.is_empty(), "bullets", out);
            text_fit(&s.bullets, 1184.0, 520.0, 26.0, &at("bullets"), out);
        }
        Layout::TwoColumn => {
            need(!s.left.is_empty(), "left", out);
            need(!s.right.is_empty(), "right", out);
            text_fit(&s.left, 570.0, 520.0, 24.0, &at("left"), out);
            text_fit(&s.right, 570.0, 520.0, 24.0, &at("right"), out);
        }
        Layout::Image | Layout::ImageText => {
            need(s.image.is_some(), "image", out);
            if s.layout == Layout::ImageText {
                text_fit(&s.bullets, 560.0, 520.0, 24.0, &at("bullets"), out);
            }
        }
        Layout::Table => {
            need(s.table.is_some(), "table", out);
            if let Some(t) = &s.table {
                if t.rows.len() > 12 {
                    out.push(Issue::warn(
                        &at("table"),
                        format!(
                            "{} rows will not fit; split the table over two slides (12 rows max)",
                            t.rows.len()
                        ),
                    ));
                }
                if t.columns() > 7 {
                    out.push(Issue::warn(
                        &at("table"),
                        "more than 7 columns will be hard to read",
                    ));
                }
            }
        }
        Layout::Chart => {
            need(s.chart.is_some(), "chart", out);
            if let Some(c) = &s.chart {
                check_chart(c, &at("chart"), out);
            }
        }
        Layout::Quote => need(!s.quote.is_empty(), "quote", out),
        Layout::BigNumber => need(!s.number.is_empty(), "number", out),
        Layout::Free => {
            if s.elements.is_empty() {
                out.push(Issue::warn(
                    &at("elements"),
                    "free slide without elements is empty",
                ));
            }
            for (j, e) in s.elements.iter().enumerate() {
                let p = at(&format!("elements/{j}"));
                if e.x < 0.0
                    || e.y < 0.0
                    || e.x + e.w > deck::WIDTH + 0.5
                    || e.y + e.h > deck::HEIGHT + 0.5
                {
                    out.push(Issue::warn(&p, "element goes past the 1280×720 slide"));
                }
                if e.kind == "text" && !e.text.is_empty() {
                    let size = e.size.unwrap_or(24.0);
                    if crate::model::text::plain(&e.text).chars().count() > capacity(e.w, e.h, size)
                    {
                        out.push(Issue::warn(&p, "text likely overflows its box"));
                    }
                }
                if let Some(img) = &e.image {
                    check_image(item, img, &p, out);
                }
                if let Some(c) = &e.chart {
                    check_chart(c, &p, out);
                }
            }
        }
    }
    if let Some(img) = &s.image {
        check_image(item, img, &at("image"), out);
    }
    if s.layout != Layout::Free
        && s.title.is_empty()
        && s.layout != Layout::Quote
        && s.layout != Layout::BigNumber
    {
        out.push(Issue::warn(&at("title"), "slide has no title"));
    }
}

fn text_fit(lines: &[String], w: f64, h: f64, size: f64, path: &ElementPath, out: &mut Vec<Issue>) {
    let line_cap = capacity(w, size * 1.3, size).max(1);
    let used: usize = lines
        .iter()
        .map(|l| {
            crate::model::text::plain(l)
                .chars()
                .count()
                .div_ceil(line_cap)
                .max(1)
        })
        .sum();
    // 每行加上项目间距（约 0.35 行）。
    let budget = capacity(w, h, size) / line_cap;
    if used + lines.len() / 3 > budget {
        out.push(Issue::warn(path, format!("text likely overflows ({used} lines for about {budget}); shorten it or split the slide")));
    }
    if lines.len() > 7 {
        out.push(Issue::warn(path, "more than 7 bullets; keep slides short"));
    }
}

fn check_image(item: &WorkItem, rel: &str, path: &ElementPath, out: &mut Vec<Issue>) {
    match item.asset(rel) {
        Some(p) if p.is_file() => {}
        Some(_) => out.push(Issue::error(
            path,
            format!("image \"{rel}\" not found in the item folder"),
        )),
        None => out.push(Issue::error(
            path,
            format!("image \"{rel}\" must be a path inside the item folder"),
        )),
    }
}

fn check_chart(c: &Chart, path: &ElementPath, out: &mut Vec<Issue>) {
    if c.data.is_some() {
        return;
    }
    if c.series.is_empty() {
        out.push(Issue::error(
            path,
            "chart has no series (give \"series\" or \"data\")",
        ));
    }
    for s in &c.series {
        if c.r#type == ChartType::Scatter {
            if s.x.len() != s.values.len() {
                out.push(Issue::error(
                    path,
                    format!(
                        "scatter series \"{}\" needs as many x values as values",
                        s.name
                    ),
                ));
            }
        } else if s.values.len() != c.categories.len() {
            out.push(Issue::error(
                path,
                format!(
                    "series \"{}\" has {} values for {} categories",
                    s.name,
                    s.values.len(),
                    c.categories.len()
                ),
            ));
        }
    }
    if c.r#type.is_round() && c.series.iter().flat_map(|s| &s.values).any(|v| *v < 0.0) {
        out.push(Issue::error(
            path,
            "pie and donut charts cannot show negative values",
        ));
    }
}

fn check_book(b: &Book, out: &mut Vec<Issue>) {
    check_format(&b.format, book::FORMAT, Kind::Book, out);
    if b.sheets.is_empty() {
        out.push(Issue::error(
            &ElementPath::new("book.json", "/sheets"),
            "a book needs at least one sheet",
        ));
    }
    for s in &b.sheets {
        for key in s.cells.keys() {
            if parse_cell(key).is_none() {
                out.push(Issue::error(
                    &ElementPath::cells(&s.name, key),
                    format!("\"{key}\" is not a cell reference like B2"),
                ));
            }
        }
        let ranges = s
            .formats
            .keys()
            .chain(s.fills.keys())
            .chain(s.merges.iter())
            .chain(s.bold.iter());
        for r in ranges.chain(s.conditional.iter().map(|c| &c.range)) {
            if parse_range(r).is_none() {
                out.push(Issue::error(
                    &ElementPath::cells(&s.name, r),
                    format!("\"{r}\" is not a range like A1:C3"),
                ));
            }
        }
        for c in &s.charts {
            check_chart(&c.chart, &ElementPath::cells(&s.name, &c.anchor), out);
        }
    }
}

fn check_doc(item: &WorkItem, d: &Doc, out: &mut Vec<Issue>) {
    let blocks = d.blocks();
    if blocks.is_empty() {
        out.push(Issue::warn(
            &ElementPath::section(""),
            "the document is empty",
        ));
    }
    let mut section = String::new();
    for b in &blocks {
        match b {
            Block::Heading { id, .. } => section.clone_from(id),
            Block::Image { src, .. } if !src.starts_with("http") => {
                check_image(item, src, &ElementPath::section(&section), out);
            }
            Block::Chart(c) => check_chart(c, &ElementPath::section(&section), out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
