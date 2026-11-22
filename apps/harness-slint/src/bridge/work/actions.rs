//! User actions for Work items: template creation, Office import/export, inline sheet cell edits
//! (with `.revisions/` snapshots), folder reveal, and detecting `work/<slug>` from agent tools.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use harness_work::{create, export, import, ExportFormat, Kind, Source, WorkItem};
use serde_json::Value;

pub(super) fn template_kind(template: &str) -> (Kind, &'static str) {
    match template {
        "pitch-deck" => (Kind::Deck, "Pitch Deck"),
        "budget" => (Kind::Book, "Budget"),
        "kpi-tracker" => (Kind::Book, "KPI Tracker"),
        "weekly-report" => (Kind::Doc, "Weekly Report"),
        "meeting-notes" => (Kind::Doc, "Meeting Notes"),
        "proposal" => (Kind::Doc, "Proposal"),
        _ => (Kind::Deck, "New Deck"),
    }
}

pub(super) fn create_from_template(root: &Path, template: &str) -> Result<String, String> {
    let (kind, title) = template_kind(template);
    let item = create(root, kind, title, template).map_err(|e| e.to_string())?;
    Ok(item.slug())
}

pub(super) fn export_item(
    root: &Path,
    slug: &str,
    format_name: &str,
) -> Result<Vec<PathBuf>, String> {
    let dir = root.join(harness_work::WORK_DIR).join(slug);
    let item = WorkItem::open(&dir).map_err(|e| e.to_string())?;
    let format = if format_name.trim().is_empty() {
        *ExportFormat::for_kind(item.kind)
            .first()
            .ok_or_else(|| "No export format for this item".to_string())?
    } else {
        ExportFormat::parse(format_name)
            .ok_or_else(|| format!("Unknown export format '{format_name}'"))?
    };
    export(&item, format).map_err(|e| e.to_string())
}

pub(super) fn pick_and_import(root: &Path) -> Option<Result<(String, String), String>> {
    let file = rfd::FileDialog::new()
        .set_title("Import Office file")
        .add_filter(
            "Office files",
            &["pptx", "xlsx", "csv", "tsv", "docx", "md"],
        )
        .pick_file()?;
    Some(match import(root, &file) {
        Ok(report) => {
            let slug = report
                .dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let note = if report.warnings.is_empty() {
                format!("Imported {}", report.title)
            } else {
                format!(
                    "Imported {} ({} note(s): {})",
                    report.title,
                    report.warnings.len(),
                    report.warnings[0]
                )
            };
            Ok((slug, note))
        }
        Err(e) => Err(e.to_string()),
    })
}

/// Saves a snapshot into `.revisions/` before mutating a source file on disk (W6.4).
fn snapshot_revision(item: &WorkItem) {
    let Ok(raw) = item.read_raw() else {
        return;
    };
    let rev_dir = item.dir.join(".revisions");
    if std::fs::create_dir_all(&rev_dir).is_err() {
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let rev_file = rev_dir.join(format!("{ts}-{}", item.kind.file_name()));
    let _ = std::fs::write(rev_file, raw);
}

fn parse_cell_value(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Value::Null;
    }
    if trimmed.starts_with('=') {
        return Value::String(trimmed.to_string());
    }
    if trimmed.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return Value::from(n);
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::String(raw.to_string())
}

/// Updates one cell in `book.json` (single-writer path for hand edits, W6.2/W6.3).
pub(super) fn edit_sheet_cell(
    root: &Path,
    slug: &str,
    sheet_idx: usize,
    coord: &str,
    raw: &str,
) -> Result<(), String> {
    let dir = root.join(harness_work::WORK_DIR).join(slug);
    let item = WorkItem::open(&dir).map_err(|e| e.to_string())?;
    let Source::Book(mut book) = item.load().map_err(|e| e.to_string())? else {
        return Err("Item is not a spreadsheet".into());
    };
    let sheet = book
        .sheets
        .get_mut(sheet_idx)
        .ok_or_else(|| "Sheet index out of range".to_string())?;
    snapshot_revision(&item);
    let key = coord.trim().to_ascii_uppercase();
    if raw.trim().is_empty() {
        sheet.cells.remove(&key);
    } else {
        sheet.cells.insert(key, parse_cell_value(raw));
    }
    item.save(&Source::Book(book)).map_err(|e| e.to_string())
}

/// Extracts a `work/<slug>` folder name from a tool title when that item exists on disk (W4.1).
pub(super) fn detect_work_item(
    title: &str,
    workspace: &Path,
) -> Option<(String, String, String, String)> {
    let lower = title.to_ascii_lowercase();
    let idx = lower.find("work/").or_else(|| lower.find("work\\"))?;
    let rest = &title[idx + 5..];
    let slug: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if slug.is_empty() {
        return None;
    }
    let dir = workspace.join(harness_work::WORK_DIR).join(&slug);
    let item = WorkItem::open(&dir).ok()?;
    let summary = harness_work::scan(workspace)
        .into_iter()
        .find(|s| s.slug == slug)?;
    let kind = super::load::kind_str(item.kind).to_string();
    let detail = super::load::summary_detail(&summary);
    Some((slug, summary.title, kind, detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cell_types() {
        assert_eq!(parse_cell_value("42"), Value::from(42));
        assert_eq!(parse_cell_value("=SUM(A1:A3)"), Value::from("=SUM(A1:A3)"));
        assert_eq!(parse_cell_value("true"), Value::Bool(true));
        assert_eq!(parse_cell_value(""), Value::Null);
    }
}
