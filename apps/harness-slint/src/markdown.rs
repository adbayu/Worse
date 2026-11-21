//! Markdown → Slint blocks (spec `phase-8-ui.md` §3 "Markdown").
//!
//! Top-level headings, code blocks and tables are split out; everything between them goes through Slint's
//! `StyledText::from_markdown` (paragraphs, lists, emphasis, inline code, links). Chunks that `StyledText`
//! cannot represent fall back to plain text. Code blocks carry highlighted light/dark variants
//! (`crate::highlight`) and a file-type icon for their language header.

use std::borrow::Cow;
use std::rc::Rc;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use slint::{ModelRc, StyledText, VecModel};

use crate::files::lang_icon_index;
use crate::highlight;
use crate::ui::{MdBlock, MdLink, MdRow};

mod blocks;
mod text;

use blocks::{
    block, cell, cell_plain, code_block, dedent, fence_lang, item_marker, list_block, list_item,
    table_block, text_block,
};
pub(crate) use text::{expand_tabs, link_at};
use text::{links_model, styled, Styled};

const KIND_TEXT: i32 = 0;
const KIND_HEADING: i32 = 1;
const KIND_CODE: i32 = 2;
const KIND_TABLE: i32 = 3;
const KIND_LIST: i32 = 4;
const KIND_RULE: i32 = 5;

const INLINE_CODE_LIGHT: &str = "#a45a1f";
const INLINE_CODE_DARK: &str = "#e8b48c";
const TAB_WIDTH: usize = 4;

/// Splits `markdown` into renderable blocks. Called on every streaming flush for the growing message.
// One flat match over the pulldown-cmark events; each arm is short.
#[allow(clippy::too_many_lines)]
pub fn render(markdown: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut last_end = 0usize;
    let mut heading: Option<(u8, std::ops::Range<usize>)> = None;
    let mut code: Option<(String, String, std::ops::Range<usize>)> = None;
    // Table rows (first = header) and the table's source range start.
    let mut table: Option<(Vec<Vec<Styled>>, usize)> = None;
    // Plain cell text of the same table (CSV copy).
    let mut plain: Vec<Vec<String>> = Vec::new();
    // Top-level list: ordered start number, items, source start.
    let mut list: Option<(Option<u64>, Vec<Styled>, usize)> = None;
    let mut item: Option<(usize, usize, Option<usize>)> = None;
    let parser = Parser::new_ext(
        markdown,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    );
    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) if depth == 0 => {
                heading = Some((level as u8, range));
                depth += 1;
            }
            Event::Start(Tag::CodeBlock(kind)) if depth == 0 => {
                code = Some((fence_lang(kind), String::new(), range));
                depth += 1;
            }
            Event::Start(Tag::CodeBlock(kind)) if item.is_some() && code.is_none() => {
                if let Some((start, width, cursor)) = item.as_mut() {
                    match *cursor {
                        None => {
                            if let Some((first, items, list_start)) = list.as_mut() {
                                if let Some(last) = items.last_mut() {
                                    *last = list_item(&markdown[*start..range.start]);
                                }
                                blocks.extend(text_block(&markdown[last_end..*list_start]));
                                let done = std::mem::take(items);
                                let count = u64::try_from(done.len()).unwrap_or(0);
                                blocks.push(list_block(*first, done));
                                *first = (*first).map(|n| n + count);
                                *list_start = range.start;
                            }
                        }
                        Some(at) => {
                            blocks.extend(text_block(&dedent(&markdown[at..range.start], *width)));
                        }
                    }
                }
                last_end = range.start;
                code = Some((fence_lang(kind), String::new(), range));
                depth += 1;
            }
            Event::Start(Tag::Table(_)) if depth == 0 => {
                table = Some((Vec::new(), range.start));
                plain.clear();
                depth += 1;
            }
            Event::Start(Tag::TableHead | Tag::TableRow) if table.is_some() => {
                if let Some((rows, _)) = table.as_mut() {
                    rows.push(Vec::new());
                }
                plain.push(Vec::new());
                depth += 1;
            }
            Event::Start(Tag::TableCell) if table.is_some() => {
                if let Some((rows, _)) = table.as_mut() {
                    if let Some(row) = rows.last_mut() {
                        row.push(cell(&markdown[range.clone()]));
                    }
                }
                if let Some(row) = plain.last_mut() {
                    row.push(cell_plain(&markdown[range]));
                }
                depth += 1;
            }
            // 与 ZCode 一致（markdown-list.tsx：列表 my-3、列表项 space-y-1.5）：顶层列表拆成独立块，逐项排版。
            Event::Start(Tag::List(first)) if depth == 0 => {
                list = Some((first, Vec::new(), range.start));
                depth += 1;
            }
            Event::Start(Tag::Item) if depth == 1 && list.is_some() => {
                let source = &markdown[range.clone()];
                if let Some((_, items, _)) = list.as_mut() {
                    items.push(list_item(source));
                }
                item = Some((range.start, item_marker(source.trim_start()).1, None));
                depth += 1;
            }
            Event::Start(_) => depth += 1,
            Event::Rule if depth == 0 => {
                blocks.extend(text_block(&markdown[last_end..range.start]));
                blocks.push(block(KIND_RULE));
                last_end = range.end;
            }
            Event::Text(text) if code.is_some() => {
                if let Some((_, body, _)) = code.as_mut() {
                    body.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) if depth == 1 => {
                depth = 0;
                if let Some((level, range)) = heading.take() {
                    blocks.extend(text_block(&markdown[last_end..range.start]));
                    let inner = markdown[range.clone()]
                        .trim()
                        .trim_start_matches('#')
                        .trim_end_matches('#')
                        .trim();
                    // StyledText 不支持标题与字重属性：用加粗标记呈现标题字重，字号由 UI 按 level 设置。
                    let text = if inner.is_empty() {
                        Styled::default()
                    } else {
                        styled(&format!("**{inner}**"))
                    };
                    blocks.push(MdBlock {
                        level: i32::from(level),
                        text: text.light,
                        text_dark: text.dark,
                        plain: text.plain.into(),
                        links: links_model(text.links),
                        ..block(KIND_HEADING)
                    });
                    last_end = range.end;
                }
            }
            Event::End(TagEnd::CodeBlock) if depth == 1 => {
                depth = 0;
                if let Some((lang, body, range)) = code.take() {
                    blocks.extend(text_block(&markdown[last_end..range.start]));
                    blocks.push(code_block(&lang, &body));
                    last_end = range.end;
                }
            }
            Event::End(TagEnd::CodeBlock) if item.is_some() && code.is_some() => {
                depth = depth.saturating_sub(1);
                if let Some((lang, body, _)) = code.take() {
                    blocks.push(code_block(&lang, &body));
                }
                if let Some((_, _, cursor)) = item.as_mut() {
                    *cursor = Some(range.end);
                }
                if let Some((_, _, list_start)) = list.as_mut() {
                    *list_start = range.end;
                }
                last_end = range.end;
            }
            Event::End(TagEnd::Item) if depth == 2 && item.is_some() => {
                depth = 1;
                if let Some((_, width, Some(at))) = item.take() {
                    blocks.extend(text_block(&dedent(&markdown[at..range.end], width)));
                    if let Some((_, _, list_start)) = list.as_mut() {
                        *list_start = range.end;
                    }
                    last_end = range.end;
                }
            }
            Event::End(TagEnd::Table) if depth == 1 => {
                depth = 0;
                if let Some((rows, start)) = table.take() {
                    blocks.extend(text_block(&markdown[last_end..start]));
                    blocks.push(table_block(rows, &plain, &markdown[start..range.end]));
                    last_end = range.end;
                }
            }
            Event::End(TagEnd::List(_)) if depth == 1 && list.is_some() => {
                depth = 0;
                item = None;
                if let Some((first, items, start)) = list.take() {
                    blocks.extend(text_block(&markdown[last_end..start]));
                    if !items.is_empty() {
                        blocks.push(list_block(first, items));
                    }
                    last_end = range.end;
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                // 修复（段落间距与 ZCode 不一致）：多个段落放在同一个 StyledText 里时，段间距由 Slint 决定且偏小；
                // 每个顶层块结束时单独成块，块间距统一为 MarkdownView 的 12px（ZCode 段落 my-3）。
                if depth == 0 {
                    blocks.extend(text_block(&markdown[last_end..range.end]));
                    last_end = range.end;
                }
            }
            _ => {}
        }
    }
    // 流式输出中代码块尚未闭合时，pulldown-cmark 仍会给出 End 事件，这里只处理剩余文本。
    blocks.extend(text_block(&markdown[last_end.min(markdown.len())..]));
    blocks
}
