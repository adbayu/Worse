//! Element paths shared by comments, validation errors and hit boxes:
//! `deck.json#/slides/3/title`, `book.json#Sheet1!B2:D9`, `doc.md#pricing`.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementPath {
    /// Source file name (`deck.json`, `book.json`, `doc.md`, `chart.json`).
    pub file: String,
    /// The part after `#` (empty = whole item).
    pub fragment: String,
}

impl ElementPath {
    #[must_use]
    pub fn new(file: &str, fragment: impl Into<String>) -> Self {
        Self {
            file: file.to_string(),
            fragment: fragment.into(),
        }
    }

    #[must_use]
    pub fn slide(index: usize, field: &str) -> Self {
        let frag = if field.is_empty() {
            format!("/slides/{index}")
        } else {
            format!("/slides/{index}/{field}")
        };
        Self::new("deck.json", frag)
    }

    #[must_use]
    pub fn cells(sheet: &str, range: &str) -> Self {
        Self::new("book.json", format!("{sheet}!{range}"))
    }

    #[must_use]
    pub fn section(id: &str) -> Self {
        Self::new("doc.md", id)
    }

    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text.split_once('#') {
            Some((file, frag)) => Self::new(file.rsplit(['/', '\\']).next().unwrap_or(file), frag),
            None => Self::new(text, ""),
        }
    }

    /// Slide index for deck paths (`/slides/3/...` → 3).
    #[must_use]
    pub fn slide_index(&self) -> Option<usize> {
        self.fragment
            .strip_prefix("/slides/")?
            .split('/')
            .next()?
            .parse()
            .ok()
    }
}

impl fmt::Display for ElementPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.fragment.is_empty() {
            f.write_str(&self.file)
        } else {
            write!(f, "{}#{}", self.file, self.fragment)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let p = ElementPath::slide(3, "title");
        assert_eq!(p.to_string(), "deck.json#/slides/3/title");
        assert_eq!(ElementPath::parse("work/q3/deck.json#/slides/3/title"), p);
        assert_eq!(p.slide_index(), Some(3));
        assert_eq!(
            ElementPath::cells("Sheet1", "B2:D9").to_string(),
            "book.json#Sheet1!B2:D9"
        );
    }
}
