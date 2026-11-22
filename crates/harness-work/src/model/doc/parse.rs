//! Markdown body → [`Block`]s (pulldown-cmark events folded into a small tree).

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use super::{heading_id, inline_plain, Block, Inline};
use crate::model::chart::Chart;
use crate::model::text::Run;

#[derive(Default)]
struct Style {
    bold: u32,
    italic: u32,
}

type TableState = (Vec<Vec<Inline>>, Vec<Vec<Vec<Inline>>>, bool);

struct State {
    blocks: Vec<Block>,
    inl: Vec<Inline>,
    style: Style,
    list: Vec<bool>,
    items: Vec<(u8, Vec<Inline>)>,
    table: Option<TableState>,
    row: Vec<Vec<Inline>>,
    notes: std::collections::HashMap<String, Vec<Inline>>,
    note: Option<String>,
    link: Option<(String, String)>,
    quote: u32,
}

#[allow(clippy::too_many_lines)]
pub(super) fn parse_blocks(body: &str) -> Vec<Block> {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_STRIKETHROUGH;
    let mut st = State {
        blocks: Vec::new(),
        inl: Vec::new(),
        style: Style::default(),
        list: Vec::new(),
        items: Vec::new(),
        table: None,
        row: Vec::new(),
        notes: std::collections::HashMap::new(),
        note: None,
        link: None,
        quote: 0,
    };
    let mut code: Option<(String, String)> = None;
    let mut image: Option<(String, String)> = None;
    let mut heading: Option<(u8, String)> = None;
    for ev in Parser::new_ext(body, opts) {
        if let Some((_, text)) = code.as_mut() {
            match ev {
                Event::Text(t) => text.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    if let Some((lang, text)) = code.take() {
                        st.blocks.push(code_block(lang, text));
                    }
                }
                _ => {}
            }
            continue;
        }
        if let Some((_, cap)) = image.as_mut() {
            match ev {
                Event::Text(t) => cap.push_str(&t),
                Event::End(TagEnd::Image) => {
                    if let Some((src, caption)) = image.take() {
                        st.blocks.push(Block::Image {
                            src,
                            caption,
                            id: String::new(),
                        });
                    }
                }
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((lang, String::new()));
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                flush_para(&mut st);
                image = Some((dest_url.to_string(), String::new()));
            }
            Event::Start(Tag::Heading { level, id, .. }) => {
                heading = Some((
                    heading_level(level),
                    id.map(|s| s.to_string()).unwrap_or_default(),
                ));
            }
            Event::End(TagEnd::Heading(_)) => {
                let text = std::mem::take(&mut st.inl);
                if let Some((level, id)) = heading.take() {
                    let id = if id.is_empty() {
                        heading_id(&inline_plain(&text))
                    } else {
                        id
                    };
                    st.blocks.push(Block::Heading { level, id, text });
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if st.list.is_empty() && st.table.is_none() && st.note.is_none() && st.quote == 0 {
                    flush_para(&mut st);
                } else if st.quote > 0 || st.note.is_some() {
                    st.inl.push(Inline::Break);
                }
            }
            Event::Start(Tag::BlockQuote(_)) => st.quote += 1,
            Event::End(TagEnd::BlockQuote(_)) => {
                st.quote = st.quote.saturating_sub(1);
                let mut text = std::mem::take(&mut st.inl);
                while text.last() == Some(&Inline::Break) {
                    text.pop();
                }
                let kind = callout_kind(&mut text);
                st.blocks.push(Block::Callout { kind, text });
            }
            Event::Start(Tag::List(first)) => {
                if st.list.is_empty() {
                    flush_para(&mut st);
                } else {
                    flush_item(&mut st);
                }
                st.list.push(first.is_some());
            }
            Event::End(TagEnd::List(_)) => {
                let ordered = st.list.pop().unwrap_or(false);
                if st.list.is_empty() {
                    let items = std::mem::take(&mut st.items);
                    st.blocks.push(Block::List { ordered, items });
                }
            }
            Event::Start(Tag::Item) | Event::End(TagEnd::Item) => flush_item(&mut st),
            Event::Start(Tag::Table(_)) => st.table = Some((Vec::new(), Vec::new(), true)),
            Event::End(TagEnd::TableHead) => {
                if let Some(t) = st.table.as_mut() {
                    t.0 = std::mem::take(&mut st.row);
                    t.2 = false;
                }
            }
            Event::End(TagEnd::TableRow) => {
                let row = std::mem::take(&mut st.row);
                if let Some(t) = st.table.as_mut() {
                    t.1.push(row);
                }
            }
            Event::End(TagEnd::TableCell) => {
                let cell = std::mem::take(&mut st.inl);
                st.row.push(cell);
            }
            Event::End(TagEnd::Table) => {
                if let Some((header, rows, _)) = st.table.take() {
                    st.blocks.push(Block::Table { header, rows });
                }
            }
            Event::Start(Tag::FootnoteDefinition(label)) => {
                flush_para(&mut st);
                st.note = Some(label.to_string());
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                let mut text = std::mem::take(&mut st.inl);
                while text.last() == Some(&Inline::Break) {
                    text.pop();
                }
                if let Some(label) = st.note.take() {
                    st.notes.insert(label, text);
                }
            }
            Event::FootnoteReference(label) => st.inl.push(Inline::Note(vec![Inline::Text(Run {
                text: format!("\u{0}{label}"),
                ..Run::default()
            })])),
            Event::Start(Tag::Strong) => st.style.bold += 1,
            Event::End(TagEnd::Strong) => st.style.bold = st.style.bold.saturating_sub(1),
            Event::Start(Tag::Emphasis) => st.style.italic += 1,
            Event::End(TagEnd::Emphasis) => st.style.italic = st.style.italic.saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) => {
                st.link = Some((String::new(), dest_url.to_string()));
            }
            Event::End(TagEnd::Link) => {
                if let Some((text, url)) = st.link.take() {
                    st.inl.push(Inline::Link { text, url });
                }
            }
            Event::Text(t) => push_text(&mut st, &t, false),
            Event::Code(t) => push_text(&mut st, &t, true),
            Event::SoftBreak => push_text(&mut st, " ", false),
            Event::HardBreak => st.inl.push(Inline::Break),
            Event::Rule => st.blocks.push(Block::Rule),
            Event::Html(h) | Event::InlineHtml(h) if h.contains("pagebreak") => {
                flush_para(&mut st);
                st.blocks.push(Block::PageBreak);
            }
            _ => {}
        }
    }
    flush_para(&mut st);
    resolve(st)
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn code_block(lang: String, text: String) -> Block {
    if lang == "chart" {
        if let Ok(chart) = serde_json::from_str::<Chart>(&text) {
            return Block::Chart(Box::new(chart));
        }
    }
    Block::Code { lang, text }
}

fn push_text(st: &mut State, text: &str, code: bool) {
    if let Some((t, _)) = st.link.as_mut() {
        t.push_str(text);
        return;
    }
    if let Some(Inline::Text(last)) = st.inl.last_mut() {
        if last.bold == (st.style.bold > 0)
            && last.italic == (st.style.italic > 0)
            && last.code == code
        {
            last.text.push_str(text);
            return;
        }
    }
    st.inl.push(Inline::Text(Run {
        text: text.to_string(),
        bold: st.style.bold > 0,
        italic: st.style.italic > 0,
        code,
    }));
}

fn flush_para(st: &mut State) {
    if st.inl.is_empty() || st.note.is_some() || st.quote > 0 {
        return;
    }
    let mut text = std::mem::take(&mut st.inl);
    // `{#fig-x}` right after an image becomes the image id.
    if let (Some(Block::Image { id, .. }), [Inline::Text(r)]) =
        (st.blocks.last_mut(), text.as_slice())
    {
        let t = r.text.trim();
        if let Some(tag) = t.strip_prefix("{#").and_then(|t| t.strip_suffix('}')) {
            *id = tag.to_string();
            return;
        }
    }
    while text.last() == Some(&Inline::Break) {
        text.pop();
    }
    st.blocks.push(Block::Para(text));
}

fn flush_item(st: &mut State) {
    let mut text = std::mem::take(&mut st.inl);
    while text.last() == Some(&Inline::Break) {
        text.pop();
    }
    if !text.is_empty() {
        let depth = u8::try_from(st.list.len().saturating_sub(1)).unwrap_or(0);
        st.items.push((depth, text));
    }
}

fn callout_kind(text: &mut Vec<Inline>) -> String {
    let Some(Inline::Text(first)) = text.first_mut() else {
        return String::new();
    };
    let t = first.text.trim_start();
    let Some(rest) = t.strip_prefix("[!") else {
        return String::new();
    };
    let Some(end) = rest.find(']') else {
        return String::new();
    };
    let kind = rest[..end].to_ascii_lowercase();
    first.text = rest[end + 1..].trim_start().to_string();
    if first.text.is_empty() {
        text.remove(0);
        if text.first() == Some(&Inline::Break) {
            text.remove(0);
        }
    }
    kind
}

/// Replaces footnote references (`\0label`) with their definitions.
fn resolve(st: State) -> Vec<Block> {
    let notes = st.notes;
    let fix = |inl: &mut Vec<Inline>| {
        for i in inl.iter_mut() {
            if let Inline::Note(body) = i {
                if let Some(Inline::Text(r)) = body.first() {
                    if let Some(label) = r.text.strip_prefix('\u{0}') {
                        *body = notes.get(label).cloned().unwrap_or_default();
                    }
                }
            }
        }
    };
    let mut blocks = st.blocks;
    for b in &mut blocks {
        match b {
            Block::Para(i) | Block::Heading { text: i, .. } | Block::Callout { text: i, .. } => {
                fix(i);
            }
            Block::List { items, .. } => items.iter_mut().for_each(|(_, i)| fix(i)),
            Block::Table { header, rows } => {
                header.iter_mut().for_each(&fix);
                rows.iter_mut().flatten().for_each(&fix);
            }
            _ => {}
        }
    }
    blocks
}
