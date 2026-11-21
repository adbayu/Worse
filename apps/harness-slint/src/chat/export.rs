//! Conversation export (decision 5A): Markdown and a self-contained HTML page of the loaded transcript.

use std::fmt::Write as _;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::bridge::TranscriptRow;

mod formats;

pub use formats::formatter_for;

/// File-name stem from a session title: ASCII letters/digits and `-`, at most 48 chars.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "session".into()
    } else {
        out
    }
}

pub fn markdown(title: &str, rows: &[TranscriptRow]) -> String {
    let mut out = format!("# {title}\n\n");
    for row in rows {
        match row.kind {
            0 => {
                let _ = write!(out, "## You\n\n{}\n\n", row.text.trim_end());
            }
            1 => {
                let _ = write!(out, "## {}\n\n{}\n\n", row.who, row.text.trim_end());
            }
            2 => {
                let _ = write!(out, "- Tool: {}\n\n", row.text);
            }
            _ => {
                let _ = write!(out, "> {}\n\n", row.text);
            }
        }
    }
    out
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

fn heading(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "h2",
        HeadingLevel::H2 => "h3",
        HeadingLevel::H3 => "h4",
        _ => "h5",
    }
}

/// Markdown → HTML for the common blocks (pulldown-cmark is built without its `html` feature).
fn render(text: &str, out: &mut String) {
    let mut in_table_head = false;
    for event in Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => out.push_str("<p>"),
                Tag::Heading { level, .. } => {
                    out.push('<');
                    out.push_str(heading(level));
                    out.push('>');
                }
                Tag::BlockQuote(_) => out.push_str("<blockquote>"),
                Tag::CodeBlock(kind) => {
                    out.push_str("<pre><code");
                    if let CodeBlockKind::Fenced(lang) = kind {
                        if !lang.is_empty() {
                            out.push_str(" class=\"language-");
                            escape(&lang, out);
                            out.push('"');
                        }
                    }
                    out.push('>');
                }
                Tag::List(Some(start)) => {
                    let _ = write!(out, "<ol start=\"{start}\">");
                }
                Tag::List(None) => out.push_str("<ul>"),
                Tag::Item => out.push_str("<li>"),
                Tag::Table(_) => out.push_str("<table>"),
                Tag::TableHead => {
                    in_table_head = true;
                    out.push_str("<thead><tr>");
                }
                Tag::TableRow => out.push_str("<tr>"),
                Tag::TableCell => out.push_str(if in_table_head { "<th>" } else { "<td>" }),
                Tag::Emphasis => out.push_str("<em>"),
                Tag::Strong => out.push_str("<strong>"),
                Tag::Strikethrough => out.push_str("<del>"),
                Tag::Link { dest_url, .. } => {
                    out.push_str("<a href=\"");
                    escape(&dest_url, out);
                    out.push_str("\">");
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => out.push_str("</p>\n"),
                TagEnd::Heading(level) => {
                    out.push_str("</");
                    out.push_str(heading(level));
                    out.push_str(">\n");
                }
                TagEnd::BlockQuote(_) => out.push_str("</blockquote>\n"),
                TagEnd::CodeBlock => out.push_str("</code></pre>\n"),
                TagEnd::List(true) => out.push_str("</ol>\n"),
                TagEnd::List(false) => out.push_str("</ul>\n"),
                TagEnd::Item => out.push_str("</li>\n"),
                TagEnd::Table => out.push_str("</tbody></table>\n"),
                TagEnd::TableHead => {
                    in_table_head = false;
                    out.push_str("</tr></thead><tbody>");
                }
                TagEnd::TableRow => out.push_str("</tr>"),
                TagEnd::TableCell => out.push_str(if in_table_head { "</th>" } else { "</td>" }),
                TagEnd::Emphasis => out.push_str("</em>"),
                TagEnd::Strong => out.push_str("</strong>"),
                TagEnd::Strikethrough => out.push_str("</del>"),
                TagEnd::Link => out.push_str("</a>"),
                _ => {}
            },
            Event::Code(t) => {
                out.push_str("<code>");
                escape(&t, out);
                out.push_str("</code>");
            }
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push_str("<br>"),
            Event::Rule => out.push_str("<hr>"),
            // 文本与原始 HTML 一律转义，导出页不执行 agent 输出里的标记。
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => escape(&t, out),
            _ => {}
        }
    }
}

const STYLE: &str = "body{font:15px/1.6 system-ui,sans-serif;max-width:760px;margin:40px auto;padding:0 16px;color:#1f2328;background:#fff}\
@media(prefers-color-scheme:dark){body{color:#e6e6e6;background:#1a1a1a}.user{background:#2a2a2a!important}pre{background:#111!important}}\
.who{font-size:13px;color:#888;margin:24px 0 4px}.user{background:#f2f2f2;border-radius:12px;padding:8px 14px;white-space:pre-wrap}\
pre{background:#f6f8fa;border-radius:8px;padding:12px;overflow:auto}code{font-family:ui-monospace,Consolas,monospace;font-size:13px}\
table{border-collapse:collapse}td,th{border:1px solid #8884;padding:4px 10px}.note{color:#9a6700}.tool{color:#888;font-size:13px}";

pub fn html(title: &str, rows: &[TranscriptRow]) -> String {
    let mut out = String::from("<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>");
    escape(title, &mut out);
    out.push_str("</title><style>");
    out.push_str(STYLE);
    out.push_str("</style></head><body>\n<h1>");
    escape(title, &mut out);
    out.push_str("</h1>\n");
    for row in rows {
        match row.kind {
            0 => {
                out.push_str("<div class=\"who\">You</div><div class=\"user\">");
                escape(row.text.trim_end(), &mut out);
                out.push_str("</div>\n");
            }
            1 => {
                out.push_str("<div class=\"who\">");
                escape(&row.who, &mut out);
                out.push_str("</div>\n<div class=\"assistant\">");
                render(&row.text, &mut out);
                out.push_str("</div>\n");
            }
            2 => {
                out.push_str("<div class=\"tool\">Tool: ");
                escape(&row.text, &mut out);
                out.push_str("</div>\n");
            }
            _ => {
                out.push_str("<p class=\"note\">");
                escape(&row.text, &mut out);
                out.push_str("</p>\n");
            }
        }
    }
    out.push_str("</body></html>\n");
    out
}
