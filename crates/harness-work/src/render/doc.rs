//! `doc.md`, `book.json` and `chart.json` → Typst source. Doc blocks get a `hw-mark` before them so
//! the renderer can turn marks into per-section hit boxes.

use std::collections::HashMap;
use std::fmt::Write as _;

use super::chart_svg;
use super::deck::Built;
use super::markup::{color, fonts, inlines, lit, PRELUDE};
use crate::item::WorkItem;
use crate::model::book::{column_name, parse_range, Book};
use crate::model::chart::Chart;
use crate::model::doc::{Block, Doc};
use crate::sheet::SheetModel;

const ACCENT: &str = "#3b6cf6";

pub(crate) fn build_doc(item: &WorkItem, doc: &Doc, accent: &str) -> Built {
    let m = &doc.meta;
    let mut out = String::from(PRELUDE);
    let mut files = HashMap::new();
    let paper = if m.paper == "letter" {
        "us-letter"
    } else {
        "a4"
    };
    let header = if m.header.is_empty() {
        "none".to_string()
    } else {
        format!(
            "align(right, text(size: 9pt, fill: luma(110))[#{}])",
            lit(&m.header)
        )
    };
    let footer = if m.footer.is_empty() {
        "context align(center, text(size: 9pt, fill: luma(110), counter(page).display()))"
            .to_string()
    } else {
        format!("context grid(columns: (1fr, auto), text(size: 9pt, fill: luma(110))[#{}], text(size: 9pt, fill: luma(110), counter(page).display()))", lit(&m.footer))
    };
    let _ = writeln!(
        out,
        "#set page(paper: \"{paper}\", margin: {}cm, header: {header}, footer: {footer})\n#set text(font: {}, size: 11pt)\n#set par(justify: true, leading: 0.7em)\n#let accent = {}\n#show heading: set block(above: 1.4em, below: 0.7em)\n#show heading.where(level: 1): set text(size: 20pt)\n#show link: set text(fill: accent)",
        m.margin.clamp(0.5, 6.0),
        fonts(&m.font),
        color(accent, ACCENT)
    );
    if m.numbering {
        out.push_str("#set heading(numbering: \"1.1.\")\n");
    }
    if m.cover {
        let _ = writeln!(out, "#page(header: none, footer: none)[#v(30%)#rect(width: 64pt, height: 6pt, radius: 3pt, fill: accent)#v(12pt)#text(size: 34pt, weight: \"bold\")[#{}]#v(8pt)#text(size: 14pt, fill: luma(110))[#{}]]", lit(&doc.title()), lit(&m.header));
    }
    if m.toc {
        out.push_str("#outline(depth: 3)\n#pagebreak(weak: true)\n");
    }
    let mut section = String::new();
    let mut n = 0usize;
    for (bi, b) in doc.blocks().iter().enumerate() {
        if let Block::Heading { id, .. } = b {
            section.clone_from(id);
            n = 0;
        }
        n += 1;
        let path = if matches!(b, Block::Heading { .. }) {
            format!("doc.md#{section}")
        } else {
            format!("doc.md#{section}/{n}")
        };
        if !matches!(b, Block::PageBreak) {
            let _ = writeln!(out, "#hw-mark({})", lit(&path));
        }
        block(&mut out, &mut files, item, b, bi);
    }
    Built { source: out, files }
}

fn block(
    out: &mut String,
    files: &mut HashMap<String, Vec<u8>>,
    item: &WorkItem,
    b: &Block,
    bi: usize,
) {
    match b {
        Block::Heading { level, text, .. } => {
            let _ = writeln!(out, "#heading(level: {level})[{}]", inlines(text));
        }
        Block::Para(text) => {
            let _ = writeln!(out, "{}\n", inlines(text));
        }
        Block::List { ordered, items } => {
            for (depth, text) in items {
                let _ = writeln!(
                    out,
                    "{}{} {}",
                    "  ".repeat(usize::from(*depth)),
                    if *ordered { "+" } else { "-" },
                    inlines(text)
                );
            }
            out.push('\n');
        }
        Block::Table { header, rows } => {
            let cols = rows
                .iter()
                .map(Vec::len)
                .chain(std::iter::once(header.len()))
                .max()
                .unwrap_or(1)
                .max(1);
            let _ = writeln!(out, "#table(columns: {cols}, inset: 6pt, stroke: 0.5pt + luma(200), fill: (x, y) => if y == 0 {{ accent.lighten(85%) }},");
            if !header.is_empty() {
                out.push_str("table.header(");
                for c in 0..cols {
                    let _ = write!(
                        out,
                        "[*{}*], ",
                        header.get(c).map(|h| inlines(h)).unwrap_or_default()
                    );
                }
                out.push_str("),\n");
            }
            for row in rows {
                for c in 0..cols {
                    let _ = write!(
                        out,
                        "[{}], ",
                        row.get(c).map(|h| inlines(h)).unwrap_or_default()
                    );
                }
            }
            out.push_str(")\n");
        }
        Block::Image { src, caption, .. } => {
            let img = match item.asset(src) {
                Some(p) if p.is_file() => format!("image({}, width: 85%)", lit(&format!("/{}", src.trim_start_matches("./")))),
                _ => format!("rect(width: 85%, height: 80pt, stroke: (dash: \"dashed\"))[#align(center + horizon)[#{}]]", lit(&format!("Missing image: {src}"))),
            };
            let cap = if caption.is_empty() {
                String::new()
            } else {
                format!(", caption: [#{}]", lit(caption))
            };
            let _ = writeln!(out, "#figure({img}{cap})");
        }
        Block::Code { lang, text } => {
            let _ = writeln!(out, "#block(fill: luma(246), inset: 8pt, radius: 4pt, width: 100%, raw({}, block: true, lang: {}))", lit(text), lit(lang));
        }
        Block::Chart(chart) => {
            let resolved = super::data::resolve_chart(item, chart);
            let name = format!("__hw/doc-chart-{bi}.svg");
            files.insert(
                name.clone(),
                chart_svg::svg(&resolved, 600.0, 320.0, &chart_svg::Style::default()).into_bytes(),
            );
            let _ = writeln!(
                out,
                "#figure(image({}, width: 100%))",
                lit(&format!("/{name}"))
            );
        }
        Block::Callout { kind, text } => {
            let title = match kind.as_str() {
                "" => String::new(),
                k => {
                    let mut c = k.chars();
                    c.next()
                        .map(|f| f.to_uppercase().chain(c).collect::<String>())
                        .unwrap_or_default()
                }
            };
            let head = if title.is_empty() {
                String::new()
            } else {
                format!("#strong({})#linebreak()", lit(&title))
            };
            let _ = writeln!(out, "#block(fill: accent.lighten(90%), inset: 10pt, radius: 4pt, width: 100%, stroke: (left: 3pt + accent))[{head}{}]", inlines(text));
        }
        Block::PageBreak => out.push_str("#pagebreak()\n"),
        Block::Rule => out.push_str("#line(length: 100%, stroke: 0.5pt + luma(180))\n"),
    }
}

/// A book as tables (PDF export and agent renders). `only` limits it to one sheet range.
pub(crate) fn build_book(item: &WorkItem, book: &Book, only: Option<(&str, &str)>) -> Built {
    let mut out = String::from(PRELUDE);
    let mut files = HashMap::new();
    out.push_str("#set page(paper: \"a4\", flipped: true, margin: 1.2cm)\n#set text(font: (\"Segoe UI\", \"Arial\", \"DejaVu Sans\", \"Libertinus Serif\"), size: 9pt)\n");
    let Ok(model) = SheetModel::from_book(book) else {
        out.push_str("#\"The book could not be evaluated.\"");
        return Built { source: out, files };
    };
    for (i, sheet) in book.sheets.iter().enumerate() {
        let Ok(idx) = u32::try_from(i) else { continue };
        if only.is_some_and(|(s, _)| !s.is_empty() && !s.eq_ignore_ascii_case(&sheet.name))
            || (only.is_some_and(|(s, _)| s.is_empty()) && i > 0)
        {
            continue;
        }
        let (rows, cols) = model.used(idx);
        let (r1, c1, r2, c2) =
            only.and_then(|(_, r)| parse_range(r))
                .unwrap_or((1, 1, rows.min(500), cols.min(26)));
        let _ = writeln!(
            out,
            "#hw-mark({})\n#heading(level: 2)[#{}]",
            lit(&format!("book.json#{}!A1", sheet.name)),
            lit(&sheet.name)
        );
        let _ = write!(out, "#table(columns: {}, inset: 4pt, stroke: 0.4pt + luma(210), align: (x, y) => if x == 0 {{ center }} else {{ left }},\n[], ", c2 - c1 + 2);
        for c in c1..=c2 {
            let _ = write!(
                out,
                "table.cell(fill: luma(240))[#{}], ",
                lit(&column_name(c))
            );
        }
        let grid = model.grid(idx, r1, c1, r2 - r1 + 1, c2 - c1 + 1);
        for (ri, row) in grid.rows.iter().enumerate() {
            let _ = write!(
                out,
                "table.cell(fill: luma(240))[#\"{}\"], ",
                r1 + i32::try_from(ri).unwrap_or(0)
            );
            for cell in row {
                let fill = cell
                    .fill
                    .as_deref()
                    .map_or_else(|| "none".into(), |f| color(f, "#ffffff"));
                let body = if cell.bold {
                    format!("#strong({})", lit(&cell.text))
                } else {
                    format!("#{}", lit(&cell.text))
                };
                let al = if cell.number { "right" } else { "left" };
                let _ = write!(out, "table.cell(fill: {fill}, align: {al})[{body}], ");
            }
        }
        out.push_str(")\n");
        for (k, ch) in sheet.charts.iter().enumerate() {
            let chart = ch
                .chart
                .data
                .as_ref()
                .and_then(|d| super::data::from_model(&model, &ch.chart, &d.range))
                .unwrap_or_else(|| ch.chart.clone());
            let name = format!("__hw/book-{i}-{k}.svg");
            files.insert(
                name.clone(),
                chart_svg::svg(&chart, 640.0, 340.0, &chart_svg::Style::default()).into_bytes(),
            );
            let _ = writeln!(out, "#image({}, width: 60%)", lit(&format!("/{name}")));
        }
    }
    let _ = item;
    Built { source: out, files }
}

pub(crate) fn build_chart(item: &WorkItem, chart: &Chart) -> Built {
    let resolved = super::data::resolve_chart(item, chart);
    let mut files = HashMap::new();
    files.insert(
        "__hw/chart.svg".to_string(),
        chart_svg::svg(&resolved, 800.0, 450.0, &chart_svg::Style::default()).into_bytes(),
    );
    let source = format!("{PRELUDE}#set page(width: 800pt, height: 450pt, margin: 0pt)\n#hw-mark(\"chart.json\")\n#image(\"/__hw/chart.svg\", width: 100%, height: 100%)\n");
    Built { source, files }
}
