//! Creating Work items: a unique `work/<slug>/` folder filled from a built-in template
//! (`assets/templates/`), a project template (`<project>/.harness/work-templates/<name>/`) or a blank
//! source of the kind. The brand kit is applied to new decks.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::brand::load_brand;
use crate::error::{write_file, WorkError, WorkResult};
use crate::item::{to_json, Kind, Source, WorkItem};
use crate::model::book::Book;
use crate::model::chart::Chart;
use crate::model::deck::{Deck, Layout, Slide};
use crate::WORK_DIR;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateInfo {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// `builtin` or `project`.
    pub origin: String,
}

const BUILTIN: [(&str, &str, Kind, &str); 6] = [
    (
        "pitch-deck",
        "Pitch deck",
        Kind::Deck,
        include_str!("../assets/templates/pitch-deck.json"),
    ),
    (
        "weekly-report",
        "Weekly report",
        Kind::Doc,
        include_str!("../assets/templates/weekly-report.md"),
    ),
    (
        "budget",
        "Budget",
        Kind::Book,
        include_str!("../assets/templates/budget.json"),
    ),
    (
        "kpi-tracker",
        "KPI tracker",
        Kind::Book,
        include_str!("../assets/templates/kpi-tracker.json"),
    ),
    (
        "meeting-notes",
        "Meeting notes",
        Kind::Doc,
        include_str!("../assets/templates/meeting-notes.md"),
    ),
    (
        "proposal",
        "Proposal",
        Kind::Doc,
        include_str!("../assets/templates/proposal.md"),
    ),
];

fn project_templates(project: &Path) -> PathBuf {
    project.join(".harness").join("work-templates")
}

/// Built-in and project templates.
#[must_use]
pub fn templates(project: &Path) -> Vec<TemplateInfo> {
    let mut out: Vec<TemplateInfo> = BUILTIN
        .iter()
        .map(|(id, name, kind, _)| TemplateInfo {
            id: (*id).into(),
            name: (*name).into(),
            kind: *kind,
            origin: "builtin".into(),
        })
        .collect();
    if let Ok(entries) = std::fs::read_dir(project_templates(project)) {
        for e in entries.flatten() {
            if let Ok(item) = WorkItem::open(&e.path()) {
                let name = e.file_name().to_string_lossy().into_owned();
                out.push(TemplateInfo {
                    id: name.clone(),
                    name,
                    kind: item.kind,
                    origin: "project".into(),
                });
            }
        }
    }
    out
}

/// `Q3 Business Review!` → `q3-business-review`.
#[must_use]
pub fn slugify(title: &str) -> String {
    let s = crate::model::doc::heading_id(title);
    let s: String = s.chars().take(48).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        "untitled".into()
    } else {
        s
    }
}

fn unique_dir(work: &Path, slug: &str) -> PathBuf {
    let mut dir = work.join(slug);
    let mut n = 2;
    while dir.exists() {
        dir = work.join(format!("{slug}-{n}"));
        n += 1;
    }
    dir
}

/// Creates an item under `<project>/work/`. `template` is a template id or empty for a blank item.
pub fn create(project: &Path, kind: Kind, title: &str, template: &str) -> WorkResult<WorkItem> {
    let work = project.join(WORK_DIR);
    let dir = unique_dir(&work, &slugify(title));
    std::fs::create_dir_all(dir.join("assets")).map_err(|e| WorkError::io(&dir, e))?;
    if let Some((_, _, tkind, text)) = BUILTIN.iter().find(|t| t.0 == template) {
        if *tkind != kind {
            return Err(WorkError::Invalid(format!(
                "template {template} makes a {} item, not {}",
                tkind.label(),
                kind.label()
            )));
        }
        write_file(&dir.join(kind.file_name()), text.as_bytes())?;
    } else if !template.is_empty() {
        let src = WorkItem::open(&project_templates(project).join(template))?;
        if src.kind != kind {
            return Err(WorkError::Invalid(format!(
                "template {template} is a {}",
                src.kind.label()
            )));
        }
        copy_dir(&src.dir, &dir)?;
    } else {
        write_file(&dir.join(kind.file_name()), blank(kind).as_bytes())?;
    }
    let item = WorkItem { dir, kind };
    retitle(&item, &work, title)?;
    Ok(item)
}

fn blank(kind: Kind) -> String {
    match kind {
        Kind::Deck => to_json(&Deck {
            slides: vec![Slide {
                id: "s1".into(),
                layout: Layout::Title,
                ..Slide::default()
            }],
            ..Deck::default()
        })
        .unwrap_or_default(),
        Kind::Book => to_json(&Book::default()).unwrap_or_default(),
        Kind::Chart => to_json(&Chart {
            format: Some(crate::model::chart::FORMAT.into()),
            ..Chart::default()
        })
        .unwrap_or_default(),
        Kind::Doc => String::from("---\ntitle: \n---\n"),
    }
}

/// Sets the title (and the brand theme for decks) on a fresh item.
fn retitle(item: &WorkItem, work: &Path, title: &str) -> WorkResult<()> {
    match item.load()? {
        Source::Deck(mut d) => {
            d.title = title.into();
            let brand = load_brand(work);
            let had_custom = d.theme != Deck::default().theme;
            if !had_custom || std::fs::metadata(work.join("brand.json")).is_ok() {
                d.theme = brand.theme();
            }
            if !brand.logo.is_empty() {
                let from = work.join(&brand.logo);
                if let Some(name) = from.file_name().filter(|_| from.is_file()) {
                    let to = item.dir.join("assets").join(name);
                    std::fs::copy(&from, &to).map_err(|e| WorkError::io(&to, e))?;
                    d.theme.logo = Some(format!("assets/{}", name.to_string_lossy()));
                }
            }
            if let Some(first) = d
                .slides
                .first_mut()
                .filter(|s| s.layout == Layout::Title && s.title.is_empty())
            {
                first.title = title.into();
            }
            item.save(&Source::Deck(d))
        }
        Source::Book(mut b) => {
            b.title = title.into();
            item.save(&Source::Book(b))
        }
        Source::Chart(mut c) => {
            c.title = title.into();
            item.save(&Source::Chart(c))
        }
        Source::Doc(_) => {
            let text = item.read_raw()?;
            let text = match text.strip_prefix("---\ntitle:") {
                Some(rest) => {
                    let rest = rest.split_once('\n').map_or("", |(_, r)| r);
                    format!("---\ntitle: {title}\n{rest}")
                }
                None => format!("---\ntitle: {title}\n---\n{text}"),
            };
            write_file(&item.source_path(), text.as_bytes())
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> WorkResult<()> {
    std::fs::create_dir_all(to).map_err(|e| WorkError::io(to, e))?;
    for e in std::fs::read_dir(from)
        .map_err(|e| WorkError::io(from, e))?
        .flatten()
    {
        let name = e.file_name();
        if name == "exports" || name == ".revisions" {
            continue;
        }
        let dest = to.join(&name);
        if e.path().is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), &dest).map_err(|err| WorkError::io(&dest, err))?;
        }
    }
    Ok(())
}

/// Saves an item as a project template (`<project>/.harness/work-templates/<name>/`).
pub fn save_template(project: &Path, item: &WorkItem, name: &str) -> WorkResult<PathBuf> {
    let dir = project_templates(project).join(slugify(name));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| WorkError::io(&dir, e))?;
    }
    copy_dir(&item.dir, &dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests;
