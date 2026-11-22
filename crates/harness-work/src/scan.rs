//! Lists the Work items of a project (`<project>/work/*/`) for the library page and the agent.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::item::{Kind, Source, WorkItem};
use crate::WORK_DIR;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSummary {
    pub dir: PathBuf,
    pub slug: String,
    pub kind: Kind,
    pub title: String,
    /// Slides, sheets or top-level sections.
    pub parts: usize,
    /// Seconds since the Unix epoch of the last source change.
    pub modified: u64,
    /// The source failed to parse.
    pub broken: bool,
}

fn modified(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Summarizes one item.
#[must_use]
pub fn summarize(item: &WorkItem) -> ItemSummary {
    let loaded = item.load();
    let (title, parts, broken) = match &loaded {
        Ok(src) => {
            let parts = match src {
                Source::Deck(d) => d.slides.len(),
                Source::Book(b) => b.sheets.len(),
                Source::Doc(d) => d
                    .blocks()
                    .iter()
                    .filter(|b| matches!(b, crate::model::doc::Block::Heading { level: 1, .. }))
                    .count(),
                Source::Chart(_) => 1,
            };
            (item.title_of(src), parts, false)
        }
        Err(_) => (item.slug(), 0, true),
    };
    ItemSummary {
        dir: item.dir.clone(),
        slug: item.slug(),
        kind: item.kind,
        title,
        parts,
        modified: modified(&item.source_path()),
        broken,
    }
}

/// Every item under `<project>/work/`, newest first.
#[must_use]
pub fn scan(project: &Path) -> Vec<ItemSummary> {
    let Ok(entries) = std::fs::read_dir(project.join(WORK_DIR)) else {
        return Vec::new();
    };
    let mut out: Vec<ItemSummary> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let dir = e.path();
            Kind::ALL
                .into_iter()
                .find(|k| dir.join(k.file_name()).is_file())
                .map(|kind| WorkItem { dir, kind })
        })
        .map(|item| summarize(&item))
        .collect();
    out.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.slug.cmp(&b.slug))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_items_and_skips_other_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join(WORK_DIR);
        std::fs::create_dir_all(work.join("a")).unwrap();
        std::fs::create_dir_all(work.join("junk")).unwrap();
        std::fs::write(
            work.join("a").join("deck.json"),
            r#"{"format":"harness-deck/1","title":"Q3","slides":[{"layout":"title"},{"layout":"bullets"}]}"#,
        )
        .unwrap();
        let items = scan(tmp.path());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Q3");
        assert_eq!(items[0].parts, 2);
        assert!(!items[0].broken);
    }
}
