//! Self-contained HTML exports (one file, images inlined as data URLs) for sharing by mail or chat.

use std::fmt::Write as _;

use base64::Engine as _;

use super::ooxml::esc;
use crate::error::WorkResult;
use crate::item::{Source, WorkItem};
use crate::model::doc::{Block, Doc, Inline};
use crate::render::chart_svg::{svg, Style};
use crate::render::{render, Target};
use crate::sheet::SheetModel;

const CSS: &str = "body{font-family:'Segoe UI',Arial,sans-serif;max-width:860px;margin:40px auto;padding:0 20px;color:#1f2328;line-height:1.6}table{border-collapse:collapse;margin:12px 0}td,th{border:1px solid #d1d5db;padding:6px 10px}th{background:#f3f4f6}img,svg{max-width:100%}pre{background:#f6f8fa;padding:10px;border-radius:6px;overflow:auto}.callout{border-left:4px solid #3b6cf6;background:#eef2ff;padding:8px 12px;margin:12px 0}.slide{margin:0 0 24px;box-shadow:0 1px 4px #0002}.notes{color:#6b7280;font-size:14px;margin:-16px 0 24px}figure{text-align:center}figcaption{color:#6b7280;font-size:14px}";

fn page(title: &str, body: &str) -> String {
    format!("<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{}</title><style>{CSS}</style></head><body>{body}</body></html>", esc(title))
}

fn data_url(bytes: &[u8], name: &str) -> String {
    let mime = match name
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        _ => "image/png",
    };
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn inlines(items: &[Inline], notes: &mut Vec<String>) -> String {
    let mut s = String::new();
    for i in items {
        match i {
            Inline::Text(r) => {
                let mut t = esc(&r.text);
                if r.code {
                    t = format!("<code>{t}</code>");
                }
                if r.italic {
                    t = format!("<em>{t}</em>");
                }
                if r.bold {
                    t = format!("<strong>{t}</strong>");
                }
                s.push_str(&t);
            }
            Inline::Link { text, url } => {
                let _ = write!(s, "<a href=\"{}\">{}</a>", esc(url), esc(text));
            }
            Inline::Note(n) => {
                let body = inlines(n, notes);
                notes.push(body);
                let _ = write!(s, "<sup><a href=\"#fn{0}\">{0}</a></sup>", notes.len());
            }
            Inline::Break => s.push_str("<br>"),
        }
    }
    s
}

fn doc_html(item: &WorkItem, doc: &Doc) -> String {
    let mut out = String::new();
    let mut notes = Vec::new();
    for b in doc.blocks() {
        match b {
            Block::Heading { level, id, text } => {
                let _ = write!(
                    out,
                    "<h{level} id=\"{}\">{}</h{level}>",
                    esc(&id),
                    inlines(&text, &mut notes)
                );
            }
            Block::Para(t) => {
                let _ = write!(out, "<p>{}</p>", inlines(&t, &mut notes));
            }
            Block::List { ordered, items } => {
                let tag = if ordered { "ol" } else { "ul" };
                let _ = write!(out, "<{tag}>");
                for (depth, t) in items {
                    let _ = write!(
                        out,
                        "<li style=\"margin-left:{}em\">{}</li>",
                        u32::from(depth) * 2,
                        inlines(&t, &mut notes)
                    );
                }
                let _ = write!(out, "</{tag}>");
            }
            Block::Table { header, rows } => {
                out.push_str("<table>");
                if !header.is_empty() {
                    out.push_str("<tr>");
                    for h in &header {
                        let _ = write!(out, "<th>{}</th>", inlines(h, &mut notes));
                    }
                    out.push_str("</tr>");
                }
                for r in &rows {
                    out.push_str("<tr>");
                    for c in r {
                        let _ = write!(out, "<td>{}</td>", inlines(c, &mut notes));
                    }
                    out.push_str("</tr>");
                }
                out.push_str("</table>");
            }
            Block::Image { src, caption, .. } => {
                let img = item
                    .asset(&src)
                    .and_then(|p| std::fs::read(p).ok())
                    .map_or_else(
                        || format!("<em>Missing image: {}</em>", esc(&src)),
                        |b| {
                            format!(
                                "<img src=\"{}\" alt=\"{}\">",
                                data_url(&b, &src),
                                esc(&caption)
                            )
                        },
                    );
                let _ = write!(
                    out,
                    "<figure>{img}<figcaption>{}</figcaption></figure>",
                    esc(&caption)
                );
            }
            Block::Code { text, .. } => {
                let _ = write!(out, "<pre><code>{}</code></pre>", esc(&text));
            }
            Block::Chart(c) => {
                let resolved = crate::render::data::resolve_chart(item, &c);
                let _ = write!(
                    out,
                    "<figure>{}</figure>",
                    svg(&resolved, 760.0, 400.0, &Style::default())
                );
            }
            Block::Callout { kind, text } => {
                let head = if kind.is_empty() {
                    String::new()
                } else {
                    format!("<strong>{}</strong><br>", esc(&kind.to_uppercase()))
                };
                let _ = write!(
                    out,
                    "<div class=\"callout\">{head}{}</div>",
                    inlines(&text, &mut notes)
                );
            }
            Block::PageBreak | Block::Rule => out.push_str("<hr>"),
        }
    }
    if !notes.is_empty() {
        out.push_str("<hr><ol>");
        for (i, n) in notes.iter().enumerate() {
            let _ = write!(out, "<li id=\"fn{}\">{n}</li>", i + 1);
        }
        out.push_str("</ol>");
    }
    out
}

fn book_html(book: &crate::model::book::Book) -> String {
    let Ok(model) = SheetModel::from_book(book) else {
        return "<p>The book could not be evaluated.</p>".into();
    };
    let mut out = String::new();
    for (i, sheet) in book.sheets.iter().enumerate() {
        let Ok(idx) = u32::try_from(i) else { continue };
        let (rows, cols) = model.used(idx);
        let grid = model.grid(idx, 1, 1, rows.min(2000), cols.min(100));
        let _ = write!(out, "<h2>{}</h2><table>", esc(&sheet.name));
        for row in &grid.rows {
            out.push_str("<tr>");
            for c in row {
                let mut style = String::new();
                if let Some(f) = &c.fill {
                    let _ = write!(style, "background:{};", esc(f));
                }
                if c.bold {
                    style.push_str("font-weight:bold;");
                }
                if c.number {
                    style.push_str("text-align:right;");
                }
                let _ = write!(out, "<td style=\"{style}\">{}</td>", esc(&c.text));
            }
            out.push_str("</tr>");
        }
        out.push_str("</table>");
    }
    out
}

/// One self-contained HTML file for any kind.
pub(crate) fn html(item: &WorkItem, source: &Source) -> WorkResult<String> {
    let title = item.title_of(source);
    let body = match source {
        Source::Doc(d) => doc_html(item, d),
        Source::Book(b) => book_html(b),
        Source::Chart(c) => svg(
            &crate::render::data::resolve_chart(item, c),
            800.0,
            450.0,
            &Style::default(),
        ),
        Source::Deck(d) => {
            let pages = render(item, &Target::All, 1.0)?;
            let mut s = format!("<h1>{}</h1>", esc(&title));
            for p in &pages.pages {
                let _ = write!(
                    s,
                    "<img class=\"slide\" src=\"{}\" alt=\"Slide {}\">",
                    data_url(&p.png, "x.png"),
                    p.index + 1
                );
                if let Some(n) = d
                    .slides
                    .get(p.index)
                    .map(|sl| sl.notes.as_str())
                    .filter(|n| !n.is_empty())
                {
                    let _ = write!(s, "<p class=\"notes\">{}</p>", esc(n));
                }
            }
            s
        }
    };
    Ok(page(&title, &body))
}
