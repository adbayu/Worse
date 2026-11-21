use super::{
    expand_tabs, highlight, lang_icon_index, links_model, styled, CodeBlockKind, Event, MdBlock,
    MdRow, ModelRc, Parser, Rc, Styled, StyledText, VecModel, KIND_CODE, KIND_LIST, KIND_TABLE,
    KIND_TEXT,
};

pub(super) fn block(kind: i32) -> MdBlock {
    MdBlock {
        kind,
        ..MdBlock::default()
    }
}

pub(super) fn text_block(source: &str) -> Option<MdBlock> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    let text = styled(trimmed);
    Some(MdBlock {
        text: text.light,
        text_dark: text.dark,
        plain: text.plain.into(),
        links: links_model(text.links),
        ..block(KIND_TEXT)
    })
}

pub(super) fn code_block(lang: &str, body: &str) -> MdBlock {
    let code = body.trim_end_matches('\n');
    let shown = expand_tabs(code);
    let light = highlight::styled(&shown, lang, false);
    let dark = highlight::styled(&shown, lang, true);
    let highlighted = light.is_some() && dark.is_some();
    MdBlock {
        code: code.into(),
        plain: shown.as_ref().into(),
        lang: lang.into(),
        lang_icon: lang_icon_index(lang),
        highlighted,
        code_light: light.unwrap_or_default(),
        code_dark: dark.unwrap_or_default(),
        ..block(KIND_CODE)
    }
}

/// Table cell source → styled text (cell ranges include the surrounding pipes on some rows).
pub(super) fn cell(source: &str) -> Styled {
    let inner = source
        .trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .trim();
    // 修复：单元格内容单独交给 StyledText 解析时，"-"、"1." 等会被当成列表/标题/引用等块级标记（"-" 显示成圆点）；
    // 表格单元格只允许行内格式，所以转义行首的块级标记（保留 `*强调*` 等行内语法）。
    let bullet = |c: char| {
        inner
            .strip_prefix(c)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    };
    if bullet('-') || bullet('*') || bullet('+') || bullet('#') || inner.starts_with('>') {
        return styled(&format!("\\{inner}"));
    }
    if let Some(at) = inner.find(['.', ')']) {
        if at > 0 && inner[..at].bytes().all(|b| b.is_ascii_digit()) {
            return styled(&format!("{}\\{}", &inner[..at], &inner[at..]));
        }
    }
    styled(inner)
}
/// Plain text of a table cell (copy as CSV): inline Markdown markers and the pipe escapes removed.
pub(super) fn cell_plain(source: &str) -> String {
    let inner = source
        .trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .trim();
    let mut out = String::new();
    for event in Parser::new(inner) {
        match event {
            Event::Text(t) | Event::Code(t) => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => out.push(' '),
            _ => {}
        }
    }
    if out.is_empty() {
        inner.replace("\\|", "|")
    } else {
        out
    }
}

/// RFC 4180 CSV: fields with commas, quotes or line breaks are quoted.
fn csv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for row in rows {
        let fields: Vec<String> = row
            .iter()
            .map(|f| {
                if f.contains([',', '"', '\n', '\r']) {
                    format!("\"{}\"", f.replace('"', "\"\""))
                } else {
                    f.clone()
                }
            })
            .collect();
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    }
    out
}

fn styled_row(mut cells: Vec<Styled>) -> MdRow {
    let (plain, links) = match cells.first_mut() {
        Some(first) => (
            std::mem::take(&mut first.plain),
            std::mem::take(&mut first.links),
        ),
        None => (String::new(), Vec::new()),
    };
    let (light, dark): (Vec<StyledText>, Vec<StyledText>) =
        cells.into_iter().map(|c| (c.light, c.dark)).unzip();
    MdRow {
        cells: ModelRc::from(Rc::new(VecModel::from(light))),
        cells_dark: ModelRc::from(Rc::new(VecModel::from(dark))),
        plain: plain.into(),
        links: links_model(links),
    }
}

pub(super) fn table_block(rows: Vec<Vec<Styled>>, plain: &[Vec<String>], source: &str) -> MdBlock {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let rows: Vec<MdRow> = rows
        .into_iter()
        .map(|mut cells| {
            // 短行补空单元格，保证每行列数一致，表格列才能对齐。
            cells.resize_with(columns, Styled::default);
            styled_row(cells)
        })
        .collect();
    MdBlock {
        level: i32::try_from(columns).unwrap_or(i32::MAX),
        rows: ModelRc::from(Rc::new(VecModel::from(rows))),
        code: source.trim_end().into(),
        csv: csv(plain).into(),
        ..block(KIND_TABLE)
    }
}

/// One top-level list item without its marker; continuation lines lose the marker's indent.
pub(super) fn item_marker(text: &str) -> (usize, usize) {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let marker = if text.starts_with(['-', '*', '+']) {
        1
    } else if digits > 0 && text[digits..].starts_with(['.', ')']) {
        digits + 1
    } else {
        0
    };
    let rest = &text[marker..];
    let width = marker + (rest.len() - rest.trim_start_matches(' ').len()).max(1);
    (marker, width)
}

pub(super) fn dedent(text: &str, width: usize) -> String {
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        out.push_str(&line[indent.min(width)..]);
    }
    out
}

pub(super) fn list_item(source: &str) -> Styled {
    let text = source.trim_start();
    let (marker, width) = item_marker(text);
    let rest = text[marker..].trim_start_matches(' ');
    let out = match rest.split_once('\n') {
        Some((first, tail)) => format!("{}\n{}", first.trim_end_matches('\r'), dedent(tail, width)),
        None => rest.to_string(),
    };
    styled(out.trim_end())
}

pub(super) fn fence_lang(kind: CodeBlockKind<'_>) -> String {
    match kind {
        CodeBlockKind::Fenced(info) => info
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string(),
        CodeBlockKind::Indented => String::new(),
    }
}

pub(super) fn list_block(ordered: Option<u64>, items: Vec<Styled>) -> MdBlock {
    let rows: Vec<MdRow> = items
        .into_iter()
        .map(|text| styled_row(vec![text]))
        .collect();
    MdBlock {
        lang: if ordered.is_some() { "ol" } else { "ul" }.into(),
        level: i32::try_from(ordered.unwrap_or(1)).unwrap_or(1),
        rows: ModelRc::from(Rc::new(VecModel::from(rows))),
        ..block(KIND_LIST)
    }
}
