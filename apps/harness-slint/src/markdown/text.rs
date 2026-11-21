use super::{
    Cow, Event, MdLink, ModelRc, Options, Parser, Rc, StyledText, Tag, TagEnd, VecModel,
    INLINE_CODE_DARK, INLINE_CODE_LIGHT, TAB_WIDTH,
};

#[derive(Clone, Default)]
pub(super) struct Styled {
    pub(super) light: StyledText,
    pub(super) dark: StyledText,
    pub(super) plain: String,
    pub(super) links: Vec<MdLink>,
}

pub(super) fn links_model(links: Vec<MdLink>) -> ModelRc<MdLink> {
    ModelRc::from(Rc::new(VecModel::from(links)))
}

pub(crate) fn link_at(links: &ModelRc<MdLink>, offset: i32) -> slint::SharedString {
    use slint::Model;
    links
        .iter()
        .find(|link| link.start <= offset && offset < link.end)
        .map(|link| link.url)
        .unwrap_or_default()
}

fn plain_text(source: &str) -> (String, Vec<MdLink>) {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut open: Option<(usize, usize, String)> = None;
    let mut spans: Vec<(usize, usize, usize, String)> = Vec::new();
    let begin = |indent: usize, marker: Option<String>| {
        let mut text = "    ".repeat(indent);
        if let Some(marker) = marker {
            text.push_str(&marker);
        }
        text
    };
    for event in Parser::new_ext(source, Options::ENABLE_STRIKETHROUGH) {
        let indent = lists.len().saturating_sub(1);
        match event {
            Event::SoftBreak | Event::HardBreak | Event::Start(Tag::Paragraph) => {
                if let Some(done) = current.replace(begin(indent, None)) {
                    paragraphs.push(done);
                }
            }
            Event::Start(Tag::List(first)) => lists.push(first),
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                let marker = match lists.last().copied() {
                    Some(Some(n)) => format!("{n}. "),
                    _ => match indent % 3 {
                        0 => "\u{2022} ".to_string(),
                        1 => "\u{25e6} ".to_string(),
                        _ => "\u{25aa} ".to_string(),
                    },
                };
                if let Some(Some(n)) = lists.last_mut() {
                    *n += 1;
                }
                if let Some(done) = current.replace(begin(indent, Some(marker))) {
                    paragraphs.push(done);
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let at = current.as_ref().map_or(0, String::len);
                open = Some((paragraphs.len(), at, dest_url.into_string()));
            }
            Event::End(TagEnd::Link) => {
                if let Some((para, start, url)) = open.take() {
                    if para == paragraphs.len() {
                        let end = current.as_ref().map_or(0, String::len);
                        spans.push((para, start, end, url));
                    }
                }
            }
            Event::Text(text) => current.get_or_insert_with(String::new).push_str(&text),
            Event::Code(text) => current
                .get_or_insert_with(String::new)
                .push_str(&expand_tabs(&text)),
            _ => {}
        }
    }
    paragraphs.extend(current);
    let mut offsets = Vec::with_capacity(paragraphs.len());
    let mut total = 0usize;
    for paragraph in &paragraphs {
        offsets.push(total);
        total += paragraph.len() + 1;
    }
    let links = spans
        .into_iter()
        .filter_map(|(para, start, end, url)| {
            let base = *offsets.get(para)?;
            Some(MdLink {
                start: i32::try_from(base + start).ok()?,
                end: i32::try_from(base + end).ok()?,
                url: url.into(),
            })
        })
        .collect();
    (paragraphs.join("\n"), links)
}

pub(crate) fn expand_tabs(text: &str) -> Cow<'_, str> {
    if !text.contains('\t') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut column = 0usize;
    for ch in text.chars() {
        match ch {
            '\t' => {
                let spaces = TAB_WIDTH - column % TAB_WIDTH;
                out.extend(std::iter::repeat(' ').take(spaces));
                column += spaces;
            }
            '\n' => {
                out.push(ch);
                column = 0;
            }
            _ => {
                out.push(ch);
                column += 1;
            }
        }
    }
    Cow::Owned(out)
}

fn code_spans(source: &str) -> Vec<(std::ops::Range<usize>, String)> {
    if !source.contains('`') {
        return Vec::new();
    }
    Parser::new_ext(source, Options::ENABLE_STRIKETHROUGH)
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::Code(text) => Some((range, text.into_string())),
            _ => None,
        })
        .collect()
}

fn recolor(source: &str, spans: &[(std::ops::Range<usize>, String)], hex: &str) -> String {
    let mut out = String::with_capacity(source.len() + spans.len() * 32);
    let mut last = 0usize;
    for (range, text) in spans {
        if range.start < last || range.end > source.len() {
            continue;
        }
        out.push_str(&source[last..range.start]);
        let text = expand_tabs(text);
        if text.trim().is_empty() {
            out.push_str(&text);
        } else {
            out.push_str("<font color=\"");
            out.push_str(hex);
            out.push_str("\">");
            for ch in text.chars() {
                if ch.is_ascii_punctuation() {
                    out.push('\\');
                }
                out.push(ch);
            }
            out.push_str("</font>");
        }
        last = range.end;
    }
    out.push_str(&source[last..]);
    out
}

pub(super) fn styled(source: &str) -> Styled {
    let spans = code_spans(source);
    let parsed = if spans.is_empty() {
        StyledText::from_markdown(source)
            .ok()
            .map(|text| (text.clone(), text))
    } else {
        let variant = |hex: &str| StyledText::from_markdown(&recolor(source, &spans, hex)).ok();
        variant(INLINE_CODE_LIGHT).zip(variant(INLINE_CODE_DARK))
    };
    if let Some((light, dark)) = parsed {
        let (plain, links) = plain_text(source);
        Styled {
            light,
            dark,
            plain,
            links,
        }
    } else {
        let (text, (plain, links)) = match StyledText::from_markdown(source) {
            Ok(text) => (text, plain_text(source)),
            Err(_) => (
                StyledText::from_plain_text(source),
                (source.to_string(), Vec::new()),
            ),
        };
        Styled {
            dark: text.clone(),
            light: text,
            plain,
            links,
        }
    }
}
