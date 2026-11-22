//! Inline text runs: the only markup inside deck and book strings is `**bold**`, `*italic*` and
//! `` `code` ``. Every writer (Typst, pptx, docx, html) maps these runs the same way.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
}

/// Splits a string into runs. Unclosed markers are kept as plain text.
#[must_use]
pub fn runs(source: &str) -> Vec<Run> {
    let chars: Vec<char> = source.chars().collect();
    let mut out: Vec<Run> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        let (marker, len): (&str, usize) = if chars[i] == '`' {
            ("`", 1)
        } else if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            ("**", 2)
        } else if chars[i] == '*' {
            ("*", 1)
        } else {
            plain.push(chars[i]);
            i += 1;
            continue;
        };
        let start = i + len;
        let Some(end) = find(&chars, start, marker) else {
            plain.extend(&chars[i..start]);
            i = start;
            continue;
        };
        if end == start {
            plain.extend(&chars[i..start + len]);
            i = start + len;
            continue;
        }
        if !plain.is_empty() {
            out.push(Run {
                text: std::mem::take(&mut plain),
                ..Run::default()
            });
        }
        out.push(Run {
            text: chars[start..end].iter().collect(),
            bold: marker == "**",
            italic: marker == "*",
            code: marker == "`",
        });
        i = end + len;
    }
    if !plain.is_empty() || out.is_empty() {
        out.push(Run {
            text: plain,
            ..Run::default()
        });
    }
    out
}

fn find(chars: &[char], from: usize, marker: &str) -> Option<usize> {
    let m: Vec<char> = marker.chars().collect();
    (from..chars.len()).find(|&j| {
        chars[j..].starts_with(&m)
            && !(m.len() == 1 && m[0] == '*' && chars.get(j + 1) == Some(&'*'))
    })
}

/// Plain text without markers (search, thumbnails, csv).
#[must_use]
pub fn plain(source: &str) -> String {
    runs(source).into_iter().map(|r| r.text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_bold_italic_code() {
        let r = runs("Revenue up **18%** in *Q3* via `api`");
        let texts: Vec<_> = r.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["Revenue up ", "18%", " in ", "Q3", " via ", "api"]);
        assert!(r[1].bold && r[3].italic && r[5].code);
    }

    #[test]
    fn unclosed_marker_stays_plain() {
        assert_eq!(plain("5 * 3 = 15"), "5 * 3 = 15");
        assert_eq!(runs("a ** b").len(), 1);
    }
}
