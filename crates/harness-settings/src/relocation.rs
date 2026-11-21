//! Data dir resolution and relocation (spec §9.1.2, decision 17 = A).
//!
//! Base dir = `HARNESS_HOME` or `~/.harness`. The data dir is the base dir unless `<base>/relocated.json` points to a
//! directory holding the `.harness-data` marker. Relocation copies, verifies byte for byte, then switches the pointer
//! atomically; the old files are deleted only after the switch, from a manifest that survives interruption.
//! It must run while the hub is stopped: the UI schedules it ([`schedule`]) and [`apply_pending`] runs at the next
//! startup before the hub opens the store.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::error::SettingsError;
use crate::fsx;

mod fsops;

use fsops::{
    absolute, clear_dir, interrupt, list_files, now_ms, read_json, relocation_error,
    resume_cleanup, same_bytes, same_path, validate, write_json,
};

/// Pointer file in the base dir.
pub const POINTER_FILE: &str = "relocated.json";
/// Pending relocation request in the base dir.
pub const PENDING_FILE: &str = "relocate.pending.json";
/// Cleanup manifest in the base dir (old files still to delete after a switch).
pub const CLEANUP_FILE: &str = "relocated.cleanup.json";
/// Marker of a complete, verified data dir.
pub const DATA_MARKER: &str = ".harness-data";
/// Marker of an unfinished copy.
pub const COPY_MARKER: &str = ".harness-relocating";

/// Files of the base dir that belong to the relocation machinery, never copied.
const CONTROL_FILES: &[&str] = &[
    POINTER_FILE,
    PENDING_FILE,
    CLEANUP_FILE,
    COPY_MARKER,
    DATA_MARKER,
];

/// Result of [`resolve_data_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDirInfo {
    pub base: PathBuf,
    pub data_dir: PathBuf,
    /// True when `data_dir` comes from the pointer.
    pub relocated: bool,
}

/// What a relocation did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelocationReport {
    pub from: PathBuf,
    pub to: PathBuf,
    pub files_copied: usize,
    pub bytes_copied: u64,
    /// Credential-like files left in the old dir (never opened, see `fsx::is_forbidden`).
    pub skipped: Vec<PathBuf>,
    /// False when deleting old files failed; it resumes on the next [`resolve_data_dir`].
    pub cleanup_complete: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pointer {
    data_dir: PathBuf,
    #[serde(default)]
    moved_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pending {
    target: PathBuf,
    #[serde(default)]
    requested_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cleanup {
    from: PathBuf,
    files: Vec<PathBuf>,
    remove_dir: bool,
}

/// Test/gate hook: stop a relocation after a step, as if the process died there.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailPoint {
    AfterCopy,
    AfterVerify,
    AfterSwitch,
}

/// Default base dir: `HARNESS_HOME`, else `<home>/.harness`.
#[must_use]
pub fn default_base_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HARNESS_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let keys: &[&str] = if cfg!(windows) {
        &["USERPROFILE", "HOME"]
    } else {
        &["HOME"]
    };
    keys.iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join(".harness"))
}

/// Resolves the data dir for `base` and resumes an interrupted cleanup. Never fails: a broken pointer falls back to
/// the base dir (warning).
pub async fn resolve_data_dir(base: &Path) -> DataDirInfo {
    let mut info = DataDirInfo {
        base: base.to_path_buf(),
        data_dir: base.to_path_buf(),
        relocated: false,
    };
    match read_json::<Pointer>(&base.join(POINTER_FILE)).await {
        Ok(Some(pointer)) => {
            if fsx::exists(&pointer.data_dir.join(DATA_MARKER)).await {
                info.data_dir = pointer.data_dir;
                info.relocated = true;
            } else {
                tracing::warn!(target = %pointer.data_dir.display(), "relocated data dir lost its marker; using base dir");
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "relocated.json unreadable; using base dir"),
    }
    if let Err(e) = resume_cleanup(base, &info.data_dir).await {
        tracing::warn!(error = %e, "old data dir cleanup failed; will retry");
    }
    info
}

/// Validates `target` and records it as the pending relocation (applied by [`apply_pending`] at next start).
///
/// # Errors
///
/// Invalid target (relative, nested, non-empty) or I/O failure.
pub async fn schedule(base: &Path, target: &Path) -> Result<(), SettingsError> {
    let current = resolve_data_dir(base).await.data_dir;
    validate(base, &current, target).await?;
    let pending = Pending {
        target: absolute(target)?,
        requested_at: now_ms(),
    };
    write_json(&base.join(PENDING_FILE), &pending).await
}

/// The pending relocation target, if any.
pub async fn pending(base: &Path) -> Option<PathBuf> {
    read_json::<Pending>(&base.join(PENDING_FILE))
        .await
        .ok()
        .flatten()
        .map(|p| p.target)
}

/// Drops a pending relocation.
///
/// # Errors
///
/// I/O failure.
pub async fn cancel_pending(base: &Path) -> Result<(), SettingsError> {
    Ok(fsx::remove_file_opt(&base.join(PENDING_FILE)).await?)
}

/// Runs the pending relocation, if any. Call at startup before anything opens files in the data dir. The request is
/// consumed whether it succeeds or fails (a failing move is not retried on every start); the old dir stays in use
/// on failure.
///
/// # Errors
///
/// The relocation failed (nothing switched).
pub async fn apply_pending(base: &Path) -> Result<Option<RelocationReport>, SettingsError> {
    let Some(target) = pending(base).await else {
        return Ok(None);
    };
    let current = resolve_data_dir(base).await.data_dir;
    let result = relocate(base, &current, &target).await;
    cancel_pending(base).await?;
    result.map(Some)
}

/// Moves the data dir from `current` to `target` (copy → verify → marker → switch pointer → delete old files).
/// The hub must be stopped.
///
/// # Errors
///
/// Invalid target, copy or verification failure. In every error case the pointer is unchanged.
pub async fn relocate(
    base: &Path,
    current: &Path,
    target: &Path,
) -> Result<RelocationReport, SettingsError> {
    relocate_with_fail_point(base, current, target, None).await
}

/// [`relocate`] with an injected interruption (tests and the phase 9 gate).
#[doc(hidden)]
pub async fn relocate_with_fail_point(
    base: &Path,
    current: &Path,
    target: &Path,
    fail_at: Option<FailPoint>,
) -> Result<RelocationReport, SettingsError> {
    let target = absolute(target)?;
    let current = absolute(current)?;
    let base = absolute(base)?;
    validate(&base, &current, &target).await?;
    let target_is_base = same_path(&target, &base);

    // 1. Unfinished-copy marker (a retry after an interruption clears our own partial copy first).
    if !target_is_base {
        if fsx::exists(&target.join(COPY_MARKER)).await {
            clear_dir(&target).await?;
        }
        tokio::fs::create_dir_all(&target).await?;
        fsx::write_atomic(&target.join(COPY_MARKER), now_ms().to_string().as_bytes()).await?;
    }

    // 2. Copy.
    let (files, skipped) = list_files(&current).await?;
    let mut bytes_copied = 0;
    for rel in &files {
        let to = target.join(rel);
        if let Some(parent) = to.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        bytes_copied += tokio::fs::copy(current.join(rel), &to).await?;
    }
    interrupt(fail_at, FailPoint::AfterCopy)?;

    // 3. Verify byte for byte.
    for rel in &files {
        if !same_bytes(&current.join(rel), &target.join(rel)).await? {
            return Err(relocation_error(format!(
                "verification failed for {}",
                rel.display()
            )));
        }
    }
    interrupt(fail_at, FailPoint::AfterVerify)?;

    // 4. Marker of a complete data dir.
    if !target_is_base {
        fsx::write_atomic(&target.join(DATA_MARKER), now_ms().to_string().as_bytes()).await?;
        fsx::remove_file_opt(&target.join(COPY_MARKER)).await?;
    }

    // 5. Switch (the only step that changes which dir is in use).
    if target_is_base {
        fsx::remove_file_opt(&base.join(POINTER_FILE)).await?;
    } else {
        let pointer = Pointer {
            data_dir: target.clone(),
            moved_at: now_ms(),
        };
        write_json(&base.join(POINTER_FILE), &pointer).await?;
    }
    tracing::info!(from = %current.display(), to = %target.display(), files = files.len(), "data dir relocated");

    // 6. Cleanup of the old copy, resumable via the manifest.
    let mut old_files = files;
    let current_is_base = same_path(&current, &base);
    if !current_is_base {
        old_files.push(PathBuf::from(DATA_MARKER));
    }
    let cleanup = Cleanup {
        from: current.clone(),
        files: old_files,
        remove_dir: !current_is_base,
    };
    write_json(&base.join(CLEANUP_FILE), &cleanup).await?;
    interrupt(fail_at, FailPoint::AfterSwitch)?;
    let cleanup_complete = match resume_cleanup(&base, &target).await {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "old data dir cleanup failed; will retry");
            false
        }
    };
    Ok(RelocationReport {
        from: current,
        to: target,
        files_copied: cleanup.files.len() - usize::from(!current_is_base),
        bytes_copied,
        skipped,
        cleanup_complete,
    })
}

#[cfg(test)]
mod tests;
