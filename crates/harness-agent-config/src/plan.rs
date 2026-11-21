//! Write plans: every change is computed in memory first, shown as a masked unified diff, then applied atomically
//! after a conflict check (spec §9.1.3 "Write protocol").

use std::fmt;
use std::path::{Path, PathBuf};

use harness_settings::fsx;
use harness_settings::json::{detect_indent, is_secret_key, mask_secret, masked, OValue};
use serde::Serialize;

use crate::error::{ConfigError, Result};

mod diff;

pub use diff::unified_diff;

/// Keys whose map values are always masked in previews.
pub(crate) const SECRET_MAPS: &[&str] = &["env", "headers", "http_headers", "env_http_headers"];

/// What a change does to its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileOp {
    Write,
    Delete,
    /// Removes a whole directory (skills). Guarded by the content of one file in it.
    RemoveDir,
}

/// How to mask a file for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Json,
    Toml,
    Text,
}

/// One file change. `before_preview`/`after_preview`/`diff` have secrets masked; the raw contents stay private
/// (and out of `Debug`).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: PathBuf,
    pub op: FileOp,
    /// Unified diff (3 lines of context) of the masked previews; empty for a no-op.
    pub diff: String,
    pub before_preview: Option<String>,
    pub after_preview: Option<String>,
    /// Nothing to write (same content).
    pub no_op: bool,
    #[serde(skip)]
    expected: Option<String>,
    #[serde(skip)]
    content: Option<String>,
    #[serde(skip)]
    guard: Option<PathBuf>,
}

impl fmt::Debug for FileChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileChange")
            .field("path", &self.path)
            .field("op", &self.op)
            .field("no_op", &self.no_op)
            .finish_non_exhaustive()
    }
}

impl FileChange {
    /// Writes `after` to `path` (which held `before`).
    pub(crate) fn write(path: PathBuf, before: Option<String>, after: String, kind: Kind) -> Self {
        let no_op = before.as_deref() == Some(after.as_str());
        let before_preview = before.as_deref().map(|t| preview(t, kind));
        let after_preview = preview(&after, kind);
        let diff = if no_op {
            String::new()
        } else {
            unified_diff(
                &path.display().to_string(),
                before_preview.as_deref().unwrap_or(""),
                &after_preview,
            )
        };
        Self {
            path,
            op: FileOp::Write,
            diff,
            before_preview,
            after_preview: Some(after_preview),
            no_op,
            expected: before,
            content: Some(after),
            guard: None,
        }
    }

    /// Deletes `path` (which held `before`; `None` = already absent → no-op).
    pub(crate) fn delete(path: PathBuf, before: Option<String>, kind: Kind) -> Self {
        let before_preview = before.as_deref().map(|t| preview(t, kind));
        let diff = before_preview
            .as_deref()
            .map(|b| unified_diff(&path.display().to_string(), b, ""))
            .unwrap_or_default();
        Self {
            path,
            op: FileOp::Delete,
            diff,
            no_op: before.is_none(),
            before_preview,
            after_preview: None,
            expected: before,
            content: None,
            guard: None,
        }
    }

    /// Removes directory `dir`; `guard` is a file inside it whose content must still be `guard_before`.
    pub(crate) fn remove_dir(dir: PathBuf, guard: PathBuf, guard_before: Option<String>) -> Self {
        let before_preview = guard_before.clone();
        let diff = before_preview
            .as_deref()
            .map(|b| unified_diff(&guard.display().to_string(), b, ""))
            .unwrap_or_default();
        Self {
            path: dir,
            op: FileOp::RemoveDir,
            diff,
            before_preview,
            after_preview: None,
            no_op: false,
            expected: guard_before,
            content: None,
            guard: Some(guard),
        }
    }

    fn guard_path(&self) -> &Path {
        self.guard.as_deref().unwrap_or(&self.path)
    }
}

/// A set of file changes shown together and applied together.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WritePlan {
    pub changes: Vec<FileChange>,
}

impl WritePlan {
    pub(crate) fn single(change: FileChange) -> Self {
        Self {
            changes: vec![change],
        }
    }

    /// True when applying would write nothing.
    #[must_use]
    pub fn is_no_op(&self) -> bool {
        self.changes.iter().all(|c| c.no_op)
    }

    /// All diffs joined (for a single preview pane).
    #[must_use]
    pub fn diff(&self) -> String {
        self.changes
            .iter()
            .map(|c| c.diff.as_str())
            .filter(|d| !d.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Result of [`apply`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOutcome {
    /// Paths actually written or removed (no-op changes are not listed).
    pub written: Vec<PathBuf>,
}

/// Applies `plan`: first checks that every file still has the content the plan was computed from
/// ([`ConfigError::Conflict`] otherwise, nothing written), then writes each change atomically.
///
/// # Errors
///
/// Conflict or I/O failure.
pub async fn apply(plan: &WritePlan) -> Result<ApplyOutcome> {
    for change in plan.changes.iter().filter(|c| !c.no_op) {
        let current = fsx::read_to_string_opt(change.guard_path()).await?;
        if current != change.expected {
            return Err(ConfigError::Conflict {
                path: change.guard_path().to_path_buf(),
            });
        }
    }
    let mut outcome = ApplyOutcome::default();
    for change in plan.changes.iter().filter(|c| !c.no_op) {
        match change.op {
            FileOp::Write => {
                let content = change.content.as_deref().unwrap_or_default();
                fsx::write_atomic(&change.path, content.as_bytes()).await?;
            }
            FileOp::Delete => fsx::remove_file_opt(&change.path).await?,
            FileOp::RemoveDir => match tokio::fs::remove_dir_all(&change.path).await {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            },
        }
        outcome.written.push(change.path.clone());
    }
    Ok(outcome)
}

/// Masked display text of a file.
pub(crate) fn preview(text: &str, kind: Kind) -> String {
    match kind {
        Kind::Text => text.to_owned(),
        Kind::Json => match OValue::parse(text) {
            Ok(value) => masked(&value, SECRET_MAPS).to_pretty(&detect_indent(text), true),
            Err(_) => "<unparseable JSON hidden>\n".into(),
        },
        Kind::Toml => match text.parse::<toml_edit::DocumentMut>() {
            Ok(mut doc) => {
                mask_toml_table(doc.as_table_mut(), false);
                doc.to_string()
            }
            Err(_) => "<unparseable TOML hidden>\n".into(),
        },
    }
}

fn mask_toml_table(table: &mut toml_edit::Table, secret_map: bool) {
    for (key, item) in table.iter_mut() {
        let child_secret = secret_map || SECRET_MAPS.contains(&key.get());
        let secret_key = is_secret_key(key.get());
        mask_toml_item(item, child_secret, secret_key);
    }
}

fn mask_toml_item(item: &mut toml_edit::Item, secret_map: bool, secret_key: bool) {
    match item {
        toml_edit::Item::Table(t) => mask_toml_table(t, secret_map),
        toml_edit::Item::ArrayOfTables(a) => {
            for t in a.iter_mut() {
                mask_toml_table(t, secret_map);
            }
        }
        toml_edit::Item::Value(v) => mask_toml_value(v, secret_map, secret_key),
        toml_edit::Item::None => {}
    }
}

fn mask_toml_value(value: &mut toml_edit::Value, secret_map: bool, secret_key: bool) {
    match value {
        toml_edit::Value::String(s) if secret_map || secret_key => {
            let decor = s.decor().clone();
            let mut replaced = toml_edit::Formatted::new(mask_secret(s.value()));
            *replaced.decor_mut() = decor;
            *s = replaced;
        }
        toml_edit::Value::InlineTable(t) => {
            for (key, v) in t.iter_mut() {
                let child_secret = secret_map || SECRET_MAPS.contains(&key.get());
                mask_toml_value(v, child_secret, is_secret_key(key.get()));
            }
        }
        toml_edit::Value::Array(a) => {
            for v in a.iter_mut() {
                mask_toml_value(v, secret_map, false);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
