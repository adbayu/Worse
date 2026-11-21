//! Syntax highlighting for markdown code blocks (spec `phase-8-ui.md` §3 "Markdown").
//!
//! A small single-pass tokenizer (keywords, strings, comments, numbers, function calls, type names) for the
//! languages agents print most, rendered into Slint `StyledText` through its `<font color>` markup. Colors are
//! `ZCode`'s code-preview default themes (Shiki github-light / github-dark), the same values as the `syntax-*`
//! tokens in `ui/theme/tokens.slint`. Chosen over syntect: no grammar/theme data to load (≈0 RAM and a few KB of
//! code instead of several MB of bundled syntaxes); unknown languages render as plain monospace text.

use slint::StyledText;

mod keywords;

use keywords::{C_LIKE, DATA, GO, JS, PYTHON, RUST, SHELL};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tok {
    Plain,
    Keyword,
    Str,
    Comment,
    Function,
    Type,
    Number,
}

struct Lang {
    keywords: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    backtick_strings: bool,
    /// Rust: `'a` is a lifetime unless it closes like a char literal.
    lifetimes: bool,
}

fn lang(name: &str) -> Option<Lang> {
    let c = |keywords| Lang {
        keywords,
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        backtick_strings: false,
        lifetimes: false,
    };
    let hash = |keywords| Lang {
        keywords,
        line_comments: &["#"],
        block_comment: None,
        backtick_strings: false,
        lifetimes: false,
    };
    Some(match name.to_lowercase().as_str() {
        "rust" | "rs" => Lang {
            lifetimes: true,
            ..c(RUST)
        },
        "js" | "javascript" | "jsx" | "ts" | "typescript" | "tsx" | "mjs" | "cjs" => Lang {
            backtick_strings: true,
            ..c(JS)
        },
        "go" | "golang" => Lang {
            backtick_strings: true,
            ..c(GO)
        },
        "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "java" | "kotlin" | "cs" | "csharp"
        | "swift" => c(C_LIKE),
        "json" | "jsonc" | "json5" => c(DATA),
        "py" | "python" => hash(PYTHON),
        "sh" | "bash" | "zsh" | "shell" | "console" | "ps1" | "powershell" => hash(SHELL),
        "toml" | "yaml" | "yml" | "ini" => hash(DATA),
        _ => return None,
    })
}

/// Appends a token, merging it into the previous one when both have the same kind (tokens are produced in
/// order and are contiguous in `line`).
fn push<'a>(line: &'a str, out: &mut Vec<(Tok, &'a str)>, tok: Tok, s: &'a str) {
    if s.is_empty() {
        return;
    }
    if let Some((last, text)) = out.last_mut() {
        if *last == tok {
            // 相邻同类片段合并，减少颜色标签数量（两者在 line 中首尾相接）。
            let start = text.as_ptr() as usize - line.as_ptr() as usize;
            *text = &line[start..start + text.len() + s.len()];
            return;
        }
    }
    out.push((tok, s));
}

/// End of a quoted string starting at `i` (exclusive) and whether it was closed on this line.
fn quote_end(bytes: &[u8], i: usize, quote: u8) -> (usize, bool) {
    let mut j = i + 1;
    while j < bytes.len() {
        if bytes[j] == b'\\' {
            j += 2;
            continue;
        }
        if bytes[j] == quote {
            return (j + 1, true);
        }
        j += 1;
    }
    (bytes.len(), false)
}

/// End of an identifier (or, with `number`, a numeric literal) starting at `i`.
fn word_end(bytes: &[u8], i: usize, number: bool) -> usize {
    let mut j = i;
    while j < bytes.len()
        && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || (number && bytes[j] == b'.'))
    {
        j += 1;
    }
    j
}

fn word_tok(word: &str, next: Option<u8>, lang: &Lang) -> Tok {
    if lang.keywords.contains(&word) {
        Tok::Keyword
    } else if next == Some(b'(') || (next == Some(b'!') && lang.lifetimes) {
        Tok::Function
    } else if word.len() > 1 && word.as_bytes()[0].is_ascii_uppercase() {
        Tok::Type
    } else {
        Tok::Plain
    }
}

/// Splits one line into tokens. `in_block` carries an open block comment across lines.
fn tokenize<'a>(line: &'a str, lang: &Lang, in_block: &mut bool) -> Vec<(Tok, &'a str)> {
    let mut out: Vec<(Tok, &'a str)> = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    if *in_block {
        let Some((_, close)) = lang.block_comment else {
            *in_block = false;
            return vec![(Tok::Plain, line)];
        };
        let Some(end) = line.find(close) else {
            return vec![(Tok::Comment, line)];
        };
        i = end + close.len();
        push(line, &mut out, Tok::Comment, &line[..i]);
        *in_block = false;
    }
    while i < bytes.len() {
        let rest = &line[i..];
        if lang.line_comments.iter().any(|p| rest.starts_with(p)) {
            push(line, &mut out, Tok::Comment, rest);
            break;
        }
        if let Some((open, close)) = lang.block_comment {
            if let Some(after) = rest.strip_prefix(open) {
                if let Some(end) = after.find(close) {
                    let len = open.len() + end + close.len();
                    push(line, &mut out, Tok::Comment, &rest[..len]);
                    i += len;
                } else {
                    push(line, &mut out, Tok::Comment, rest);
                    *in_block = true;
                    i = bytes.len();
                }
                continue;
            }
        }
        let c = bytes[i];
        if c == b'"' || c == b'\'' || (c == b'`' && lang.backtick_strings) {
            let (j, closed) = quote_end(bytes, i, c);
            // Rust 生命周期 `'a`：没有在几个字符内闭合的单引号不是字符字面量。
            if c == b'\'' && lang.lifetimes && (!closed || j - i > 6) {
                push(line, &mut out, Tok::Plain, &line[i..=i]);
                i += 1;
            } else {
                push(line, &mut out, Tok::Str, &line[i..j]);
                i = j;
            }
        } else if c.is_ascii_digit() {
            let j = word_end(bytes, i, true);
            push(line, &mut out, Tok::Number, &line[i..j]);
            i = j;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let j = word_end(bytes, i, false);
            let tok = word_tok(&line[i..j], bytes.get(j).copied(), lang);
            push(line, &mut out, tok, &line[i..j]);
            i = j;
        } else {
            // 非 ASCII 或标点：按 UTF-8 字符整体推进。
            let width = rest.chars().next().map_or(1, char::len_utf8);
            push(line, &mut out, Tok::Plain, &line[i..i + width]);
            i += width;
        }
    }
    out
}
fn color(tok: Tok, dark: bool) -> Option<&'static str> {
    // 与 tokens.slint 的 syntax-* 相同（github-light / github-dark）。
    Some(match (tok, dark) {
        (Tok::Plain, _) => return None,
        (Tok::Keyword, false) => "#cf222e",
        (Tok::Keyword, true) => "#ff7b72",
        (Tok::Str, false) => "#0a3069",
        (Tok::Str, true) => "#a5d6ff",
        (Tok::Comment, false) => "#6e7781",
        (Tok::Comment, true) => "#8b949e",
        (Tok::Function, false) => "#8250df",
        (Tok::Function, true) => "#d2a8ff",
        (Tok::Type, false) => "#953800",
        (Tok::Type, true) => "#ffa657",
        (Tok::Number, false) => "#0550ae",
        (Tok::Number, true) => "#79c0ff",
    })
}

/// Escapes text for Slint's markdown parser: ASCII punctuation is backslash-escaped, indentation becomes
/// no-break spaces (markdown would otherwise strip it or read it as an indented code block).
fn escape(text: &str, leading: &mut bool, out: &mut String) {
    for ch in text.chars() {
        if *leading && (ch == ' ' || ch == '\t') {
            out.push_str(if ch == '\t' {
                "\u{a0}\u{a0}\u{a0}\u{a0}"
            } else {
                "\u{a0}"
            });
            continue;
        }
        *leading = false;
        if ch.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(ch);
    }
}

/// Markdown source (one paragraph per code line) with `<font color>` spans; `None` for unknown languages.
fn markup(code: &str, lang_name: &str, dark: bool) -> Option<String> {
    let lang = lang(lang_name)?;
    let mut out = String::with_capacity(code.len() * 2);
    let mut in_block = false;
    for (index, line) in code.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let line = line.trim_end_matches('\r');
        let mut leading = true;
        let before = out.len();
        for (tok, text) in tokenize(line, &lang, &mut in_block) {
            match color(tok, dark) {
                Some(hex) if !text.trim().is_empty() => {
                    // 缩进留在标签外，避免行首空白落在颜色标签里被当作普通空格吞掉。
                    let trimmed = text.trim_start();
                    escape(&text[..text.len() - trimmed.len()], &mut leading, &mut out);
                    out.push_str("<font color=\"");
                    out.push_str(hex);
                    out.push_str("\">");
                    escape(trimmed, &mut leading, &mut out);
                    out.push_str("</font>");
                }
                _ => escape(text, &mut leading, &mut out),
            }
        }
        if out.len() == before {
            // 空行也要保留为一个段落，否则行距会丢失。
            out.push('\u{a0}');
        }
    }
    Some(out)
}

/// Highlighted code as styled text for the light or dark theme; `None` when the language is not known
/// (the UI then shows the plain code string).
pub fn styled(code: &str, lang_name: &str, dark: bool) -> Option<StyledText> {
    let source = markup(code, lang_name, dark)?;
    StyledText::from_markdown(&source).ok()
}
