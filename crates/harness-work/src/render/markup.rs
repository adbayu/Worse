//! Safe Typst markup from source text. Every user string becomes a Typst string literal
//! (`#"..."`), so nothing an agent writes can run as Typst code.

use std::fmt::Write as _;

use crate::model::doc::Inline;
use crate::model::text::{runs, Run};

/// Typst string literal.
#[must_use]
pub fn lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `rgb("#rrggbb")` for a valid hex color, else the fallback.
#[must_use]
pub fn color(hex: &str, fallback: &str) -> String {
    let h = hex.trim();
    let ok = h.starts_with('#')
        && matches!(h.len(), 4 | 7 | 9)
        && h[1..].chars().all(|c| c.is_ascii_hexdigit());
    format!("rgb(\"{}\")", if ok { h } else { fallback })
}

fn run(r: &Run) -> String {
    let t = lit(&r.text);
    match (r.code, r.bold, r.italic) {
        (true, _, _) => format!("#raw({t})"),
        (_, true, true) => format!("#strong(emph({t}))"),
        (_, true, false) => format!("#strong({t})"),
        (_, false, true) => format!("#emph({t})"),
        _ => format!("#{t}"),
    }
}

/// Markup content for a source string with `**bold**`/`*italic*`/`` `code` `` runs.
#[must_use]
pub fn text(src: &str) -> String {
    runs(src).iter().map(run).collect()
}

/// Content block `[...]` for a source string.
#[must_use]
pub fn content(src: &str) -> String {
    format!("[{}]", text(src))
}

/// Markup for parsed doc inlines.
#[must_use]
pub fn inlines(items: &[Inline]) -> String {
    let mut s = String::new();
    for i in items {
        match i {
            Inline::Text(r) => s.push_str(&run(r)),
            Inline::Link { text, url } => {
                let _ = write!(s, "#link({})[#{}]", lit(url), lit(text));
            }
            Inline::Note(body) => {
                let _ = write!(s, "#footnote[{}]", inlines(body));
            }
            Inline::Break => s.push_str("#linebreak()"),
        }
    }
    s
}

/// A font list with sensible fallbacks on every OS, ending with the bundled serif.
#[must_use]
pub fn fonts(primary: &str) -> String {
    let mut list = vec![primary.to_string()];
    for f in [
        "Segoe UI",
        "Helvetica Neue",
        "Arial",
        "Liberation Sans",
        "DejaVu Sans",
        "Libertinus Serif",
    ] {
        if !list.iter().any(|x| x.eq_ignore_ascii_case(f)) {
            list.push(f.to_string());
        }
    }
    let quoted: Vec<String> = list.iter().map(|f| lit(f)).collect();
    format!("({})", quoted.join(", "))
}

/// Helpers every generated source starts with: `hw-hit` records a hit box (and an overflow flag)
/// as metadata that the renderer reads back; `region` places a box and flags overflowing text.
pub const PRELUDE: &str = r"
#let hw-hit(path, w, h, over: false) = context {
  let p = here().position()
  [#metadata((path: path, page: p.page, x: p.x / 1pt, y: p.y / 1pt, w: w / 1pt, h: h / 1pt, over: over)) <hw-hit>]
}
#let hw-mark(path) = context {
  let p = here().position()
  [#metadata((path: path, page: p.page, x: p.x / 1pt, y: p.y / 1pt, w: 0, h: 0, over: false)) <hw-hit>]
}
#let region(path, x, y, w, h, body, align-to: top + left) = place(top + left, dx: x, dy: y,
  block(width: w, height: h, {
    context {
      let m = measure(block(width: w, body))
      hw-hit(path, w, h, over: m.height > h + 1pt)
    }
    align(align-to, body)
  }))
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_everything() {
        assert_eq!(lit("a\"b\\c#[x]"), "\"a\\\"b\\\\c#[x]\"");
        assert_eq!(text("**hi** #code"), "#strong(\"hi\")#\" #code\"");
        assert_eq!(color("#zzz", "#000000"), "rgb(\"#000000\")");
    }
}
