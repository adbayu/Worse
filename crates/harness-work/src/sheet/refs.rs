//! Cell references inside a formula (`B2`, `$C$4:D9`, `'Sheet 2'!A1`), for `trace` and audits.

use crate::model::book::parse_cell;

/// One reference: sheet name when given, and the inclusive rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ref {
    pub sheet: Option<String>,
    pub r1: i32,
    pub c1: i32,
    pub r2: i32,
    pub c2: i32,
}

impl Ref {
    pub(crate) fn contains(&self, r: i32, c: i32) -> bool {
        (self.r1..=self.r2).contains(&r) && (self.c1..=self.c2).contains(&c)
    }
}

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$'
}

/// A cell token (`$A$1`) at the start of `s`: its length and position.
fn cell_at(s: &str) -> Option<(usize, (i32, i32))> {
    let len = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '$'))
        .map_or(s.len(), |(i, _)| i);
    parse_cell(&s[..len]).map(|cell| (len, cell))
}

/// All cell and range references of a formula; strings and function names are skipped.
pub(crate) fn refs(formula: &str) -> Vec<Ref> {
    let chars: Vec<(usize, char)> = formula.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (pos, ch) = chars[i];
        if ch == '"' {
            // 跳过字符串字面量（"" 为转义引号）。
            i += 1;
            while i < chars.len() {
                if chars[i].1 == '"' {
                    if chars.get(i + 1).is_some_and(|c| c.1 == '"') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        let prev_word = i > 0 && word_char(chars[i - 1].1);
        if prev_word || !(ch.is_ascii_alphabetic() || ch == '$' || ch == '\'') {
            i += 1;
            continue;
        }
        let rest = &formula[pos..];
        // Optional sheet prefix.
        let (sheet, after) = if let Some(q) = rest.strip_prefix('\'') {
            match q.find("'!") {
                Some(end) => (Some(q[..end].replace("''", "'")), pos + 1 + end + 2),
                None => (None, pos),
            }
        } else {
            let word = rest
                .char_indices()
                .find(|(_, c)| !word_char(*c))
                .map_or(rest.len(), |(j, _)| j);
            if rest[word..].starts_with('!') {
                (Some(rest[..word].to_string()), pos + word + 1)
            } else {
                (None, pos)
            }
        };
        let tail = &formula[after..];
        let Some((len, (r1, c1))) = cell_at(tail) else {
            // Not a reference: skip the whole word (function or name).
            let skip = rest
                .chars()
                .take_while(|c| word_char(*c) || *c == '\'')
                .count()
                .max(1);
            i += skip;
            continue;
        };
        let mut end = after + len;
        let (mut r2, mut c2) = (r1, c1);
        if let Some(t) = formula[end..].strip_prefix(':') {
            if let Some((l2, (r, c))) = cell_at(t) {
                (r2, c2) = (r, c);
                end += 1 + l2;
            }
        }
        let next = formula[end..].chars().next();
        if next.is_some_and(|c| c == '(' || c.is_ascii_alphanumeric() || c == '_') {
            i = chars
                .iter()
                .position(|(p, _)| *p >= end)
                .unwrap_or(chars.len());
            continue;
        }
        out.push(Ref {
            sheet,
            r1: r1.min(r2),
            c1: c1.min(c2),
            r2: r1.max(r2),
            c2: c1.max(c2),
        });
        i = chars
            .iter()
            .position(|(p, _)| *p >= end)
            .unwrap_or(chars.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_references() {
        let r = refs("=SUM($B$2:B9)+'My Sheet'!C3*LOG10(A1)+\"D4\"+Data!E5");
        let names: Vec<_> = r
            .iter()
            .map(|x| (x.sheet.clone(), x.r1, x.c1, x.r2, x.c2))
            .collect();
        assert_eq!(
            names,
            vec![
                (None, 2, 2, 9, 2),
                (Some("My Sheet".into()), 3, 3, 3, 3),
                (None, 1, 1, 1, 1),
                (Some("Data".into()), 5, 5, 5, 5),
            ]
        );
    }
}
