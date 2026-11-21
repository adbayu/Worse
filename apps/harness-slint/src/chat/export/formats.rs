use crate::bridge::TranscriptRow;

pub trait TranscriptFormatter: Sync {
    fn id(&self) -> &'static str;
    fn extension(&self) -> &'static str;
    fn format(&self, title: &str, rows: &[TranscriptRow]) -> String;
}

struct Markdown;

impl TranscriptFormatter for Markdown {
    fn id(&self) -> &'static str {
        "markdown"
    }

    fn extension(&self) -> &'static str {
        "md"
    }

    fn format(&self, title: &str, rows: &[TranscriptRow]) -> String {
        super::markdown(title, rows)
    }
}

struct Html;

impl TranscriptFormatter for Html {
    fn id(&self) -> &'static str {
        "html"
    }

    fn extension(&self) -> &'static str {
        "html"
    }

    fn format(&self, title: &str, rows: &[TranscriptRow]) -> String {
        super::html(title, rows)
    }
}

static FORMATTERS: [&dyn TranscriptFormatter; 2] = [&Markdown, &Html];

#[must_use]
pub fn formatter_for(id: &str) -> &'static dyn TranscriptFormatter {
    FORMATTERS
        .iter()
        .copied()
        .find(|f| f.id() == id || f.extension() == id)
        .unwrap_or(FORMATTERS[0])
}
