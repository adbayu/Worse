use super::{ToolContent, ToolLocation};

/// Line range and file of a tool row, for the chip on the row ("L120–180" or "+3 −1").
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RowFile {
    pub path: String,
    pub line: i32,
    pub range: String,
}

/// File and line range a tool call touched, from its content and locations only (no disk access, so loading a
/// long session stays cheap).
pub(crate) fn row_file(
    kind: Option<&str>,
    content: &[ToolContent],
    locations: &[ToolLocation],
) -> RowFile {
    for item in content {
        if let ToolContent::Diff {
            path,
            old_text,
            new_text,
        } = item
        {
            let added = new_text.lines().count();
            let removed = old_text.as_deref().map_or(0, |o| o.lines().count());
            let line = locations
                .iter()
                .find(|l| l.path == *path)
                .and_then(|l| l.line)
                .unwrap_or(0);
            let range = if line > 0 && added > 0 {
                span(line, line + u32::try_from(added).unwrap_or(1) - 1)
            } else {
                format!("+{added} −{removed}")
            };
            return RowFile {
                path: path.clone(),
                line: i32::try_from(line).unwrap_or(0),
                range,
            };
        }
    }
    let Some(location) = locations.first() else {
        return RowFile::default();
    };
    let reading = kind.map_or(true, |k| k == "read");
    let (from, to) = read_span(content, location.line);
    let range = match (reading, from, to) {
        (true, f, t) if f > 0 && t >= f => span(f, t),
        (_, f, _) if f > 0 => format!("L{f}"),
        _ => String::new(),
    };
    RowFile {
        path: location.path.clone(),
        line: i32::try_from(from).unwrap_or(0),
        range,
    }
}

fn span(from: u32, to: u32) -> String {
    if to > from {
        format!("L{from}–{to}")
    } else {
        format!("L{from}")
    }
}

/// Lines a read covered: the numbered lines of its output (`   120→text` or `  120\ttext`), else the location
/// line and the number of output lines.
pub(super) fn read_span(content: &[ToolContent], line: Option<u32>) -> (u32, u32) {
    let mut first = 0u32;
    let mut last = 0u32;
    let mut count = 0u32;
    for item in content {
        if let ToolContent::Text { text } = item {
            for row in text.lines() {
                count += 1;
                let trimmed = row.trim_start();
                let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
                if digits.is_empty() || digits.len() > 7 {
                    continue;
                }
                let rest = &trimmed[digits.len()..];
                if !(rest.starts_with('→') || rest.starts_with('\t')) {
                    continue;
                }
                if let Ok(n) = digits.parse::<u32>() {
                    if first == 0 {
                        first = n;
                    }
                    last = last.max(n);
                }
            }
        }
    }
    if first > 0 {
        return (first, last);
    }
    match line {
        Some(l) if l > 0 => (l, if count > 1 { l + count - 1 } else { l }),
        _ if count > 0 => (1, count),
        _ => (0, 0),
    }
}
