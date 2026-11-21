//! Project instructions (`AGENTS.md`, or `CLAUDE.md` as a fallback) appended to the system prompt.
//!
//! The files are read once per prompt turn, so the system prompt stays byte-identical across the model
//! requests of that turn (prompt caching relies on a stable prefix).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Total budget of the project instructions of one turn.
pub(crate) const BUDGET: usize = 32 * 1024;
/// Note appended to content cut at [`BUDGET`].
const TRUNCATED: &str = "\n[truncated]";
/// A `CLAUDE.md` holding only this line just imports the `AGENTS.md` next to it.
const IMPORT_LINE: &str = "@AGENTS.md";
/// Closing line of the block: tells the model the pointers are current.
const CLOSING: &str = "These instructions and the code map above are current: use their file pointers and go straight to the relevant files instead of exploring the codebase.";

/// Prompt block with the project instructions that apply to `cwd`; empty when the project has none.
///
/// Files come outermost first (repository root → `cwd`), so the closest file wins where they disagree.
pub(crate) fn instructions(cwd: &str) -> String {
    let cwd = Path::new(cwd);
    if cwd.as_os_str().is_empty() {
        return String::new();
    }
    let mut text = String::new();
    let mut budget = BUDGET;
    for dir in search_path(cwd) {
        let Some(file) = doc_file(&dir) else { continue };
        let Ok(body) = std::fs::read_to_string(&file) else {
            continue;
        };
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let cut = body.len() > budget;
        let body = if cut {
            &body[..boundary(body, budget)]
        } else {
            body
        };
        let _ = write!(
            text,
            "\n\n# Project instructions ({})\n{body}",
            display(cwd, &file)
        );
        if cut {
            text.push_str(TRUNCATED);
            break;
        }
        budget -= body.len();
        if budget == 0 {
            break;
        }
    }
    if text.is_empty() {
        return String::new();
    }
    text.push_str("\n\n");
    text.push_str(CLOSING);
    text
}

/// `cwd` and its parents, up to and including the first directory holding `.git` (file or dir) or the
/// filesystem root; outermost first.
fn search_path(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in cwd.ancestors() {
        if dir.as_os_str().is_empty() {
            break;
        }
        dirs.push(dir.to_path_buf());
        if dir.join(".git").exists() {
            break;
        }
    }
    dirs.reverse();
    dirs
}

/// `AGENTS.md` in `dir`, else `CLAUDE.md` unless it only holds its `@AGENTS.md` import line.
fn doc_file(dir: &Path) -> Option<PathBuf> {
    let agents = dir.join("AGENTS.md");
    if agents.is_file() {
        return Some(agents);
    }
    let claude = dir.join("CLAUDE.md");
    if !claude.is_file() {
        return None;
    }
    let import_only = std::fs::read_to_string(&claude).is_ok_and(|text| text.trim() == IMPORT_LINE);
    if import_only {
        None
    } else {
        Some(claude)
    }
}

/// Path relative to `cwd` when inside it, absolute otherwise (forward slashes on every OS).
fn display(cwd: &Path, file: &Path) -> String {
    file.strip_prefix(cwd)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Largest char boundary `<= index`: the budget cut keeps the content valid UTF-8.
fn boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests;
