//! A Work item on disk: `work/<slug>/` with exactly one source file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{read_text, write_file, WorkError, WorkResult};
use crate::model::{book::Book, chart::Chart, deck::Deck, doc::Doc, look};

/// `book.json` size from which the look moves to `style.json` on save. Matches the 64 KB at which
/// the API agent refuses to read `book.json` directly (`harness-api-agent` `tools/files.rs`).
const BIG_BOOK: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Deck,
    Book,
    Doc,
    Chart,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Self::Deck, Self::Book, Self::Doc, Self::Chart];

    #[must_use]
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Deck => "deck.json",
            Self::Book => "book.json",
            Self::Doc => "doc.md",
            Self::Chart => "chart.json",
        }
    }

    /// Word shown in the UI ("Slides", "Sheet", "Doc", "Chart").
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Deck => "Slides",
            Self::Book => "Sheet",
            Self::Doc => "Doc",
            Self::Chart => "Chart",
        }
    }

    /// Accepts the tool names agents use (`slides`, `deck`, `sheet`, `book`, `doc`, `chart`).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "deck" | "slides" | "slide" | "presentation" => Some(Self::Deck),
            "book" | "sheet" | "sheets" | "spreadsheet" | "workbook" => Some(Self::Book),
            "doc" | "document" | "docs" => Some(Self::Doc),
            "chart" => Some(Self::Chart),
            _ => None,
        }
    }
}

/// The parsed source of an item.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Deck(Deck),
    Book(Book),
    Doc(Doc),
    Chart(Chart),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItem {
    /// The item folder (`<project>/work/<slug>`).
    pub dir: PathBuf,
    pub kind: Kind,
}

impl WorkItem {
    /// Resolves a folder, a source file, or a path inside the folder (`.../deck.json#/slides/2`).
    pub fn open(path: &Path) -> WorkResult<Self> {
        let raw = path.to_string_lossy();
        let clean = PathBuf::from(raw.split('#').next().unwrap_or_default());
        let mut dir = if clean.is_file() {
            clean.parent().map(Path::to_path_buf).unwrap_or_default()
        } else {
            clean.clone()
        };
        // A file under assets/ or exports/ → its item folder.
        for _ in 0..3 {
            if let Some(kind) = Kind::ALL
                .into_iter()
                .find(|k| dir.join(k.file_name()).is_file())
            {
                return Ok(Self { dir, kind });
            }
            match dir.parent() {
                Some(p) => dir = p.to_path_buf(),
                None => break,
            }
        }
        Err(WorkError::Invalid(format!(
            "{} is not a Work item (no deck.json, book.json, doc.md or chart.json)",
            clean.display()
        )))
    }

    #[must_use]
    pub fn source_path(&self) -> PathBuf {
        self.dir.join(self.kind.file_name())
    }

    #[must_use]
    pub fn slug(&self) -> String {
        self.dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn read_raw(&self) -> WorkResult<String> {
        read_text(&self.source_path())
    }

    pub fn load(&self) -> WorkResult<Source> {
        let path = self.source_path();
        let text = read_text(&path)?;
        let style = self.dir.join(look::FILE);
        if self.kind == Kind::Book && style.is_file() {
            let parsed = serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|e| format!("line {} column {}: {e}", e.line(), e.column()))
                .and_then(|mut v| {
                    look::merge(&mut v, &read_text(&style).unwrap_or_default());
                    serde_json::from_value::<Book>(v).map_err(|e| e.to_string())
                });
            return parsed
                .map(Source::Book)
                .map_err(|message| WorkError::Json { path, message });
        }
        Self::parse(self.kind, &text).map_err(|message| WorkError::Json { path, message })
    }

    /// Books with a `style.json`, or large enough to bloat agent reads, keep their look there.
    fn save_book(&self, book: &Book) -> WorkResult<()> {
        let style = self.dir.join(look::FILE);
        let text = to_json(book)?;
        if !style.is_file() && text.len() < BIG_BOOK {
            return write_file(&self.source_path(), text.as_bytes());
        }
        let mut lean = book.clone();
        lean.sheets
            .iter_mut()
            .for_each(crate::model::coalesce::sheet);
        let mut value =
            serde_json::to_value(&lean).map_err(|e| WorkError::Invalid(e.to_string()))?;
        let look_text = look::split(&mut value);
        write_file(&style, look_text.as_bytes())?;
        write_file(&self.source_path(), to_json(&value)?.as_bytes())
    }

    /// Parses source text of a kind; JSON errors carry line and column.
    pub fn parse(kind: Kind, text: &str) -> Result<Source, String> {
        let json = |e: serde_json::Error| format!("line {} column {}: {e}", e.line(), e.column());
        Ok(match kind {
            Kind::Deck => {
                let mut deck: Deck = serde_json::from_str(text).map_err(json)?;
                deck.fill_ids();
                Source::Deck(deck)
            }
            Kind::Book => Source::Book(serde_json::from_str(text).map_err(json)?),
            Kind::Chart => Source::Chart(serde_json::from_str(text).map_err(json)?),
            Kind::Doc => Source::Doc(Doc::parse(text)),
        })
    }

    /// Title from the source (falls back to the folder name).
    #[must_use]
    pub fn title_of(&self, source: &Source) -> String {
        let t = match source {
            Source::Deck(d) => d.title.clone(),
            Source::Book(b) => b.title.clone(),
            Source::Doc(d) => d.title(),
            Source::Chart(c) => c.title.clone(),
        };
        if t.trim().is_empty() {
            self.slug()
        } else {
            t
        }
    }

    /// Writes the JSON source (decks, books, charts) pretty printed.
    pub fn save(&self, source: &Source) -> WorkResult<()> {
        let text = match source {
            Source::Deck(d) => to_json(d)?,
            Source::Book(b) => return self.save_book(b),
            Source::Chart(c) => to_json(c)?,
            Source::Doc(_) => {
                return Err(WorkError::Invalid(
                    "doc.md is saved as text, not through save()".into(),
                ))
            }
        };
        write_file(&self.source_path(), text.as_bytes())
    }

    #[must_use]
    pub fn exports_dir(&self) -> PathBuf {
        self.dir.join("exports")
    }

    /// Resolves an asset path written in the source (relative to the item folder; never outside it).
    #[must_use]
    pub fn asset(&self, rel: &str) -> Option<PathBuf> {
        let rel = rel.trim().trim_start_matches("./");
        if rel.is_empty() || rel.contains("..") || Path::new(rel).is_absolute() {
            return None;
        }
        Some(self.dir.join(rel))
    }
}

pub(crate) fn to_json<T: Serialize>(value: &T) -> WorkResult<String> {
    serde_json::to_string_pretty(value).map_err(|e| WorkError::Invalid(e.to_string()))
}
