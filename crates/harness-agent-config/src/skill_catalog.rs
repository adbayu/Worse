//! Skill catalog parsing and install plans (spec `docs/specs/harness/skill-marketplace.md` §3.1).
//!
//! This module is deliberately network-free: it turns the bytes of the catalog index and of the repository
//! file tree into typed rows, converts a downloaded `SKILL.md` into a [`Skill`], and extends a normal
//! [`WritePlan`] with the skill's extra files. The app crate does the HTTP work and hands the resulting
//! plan to the settings worker, so there is still exactly one place that applies writes.

use std::path::{Path, PathBuf};

use harness_settings::fsx;

use crate::error::{ConfigError, Result};
use crate::frontmatter::{unquote, Doc};
use crate::md_store::{check_segment, plan_skill_upsert};
use crate::plan::{FileChange, Kind, WritePlan};
use crate::types::Skill;

/// The file every skill directory is keyed by (same name `md_store` uses).
const SKILL_FILE: &str = "SKILL.md";
/// Front matter keys `md_store` owns; everything else is carried over verbatim.
const SKILL_KNOWN: &[&str] = &["name", "description"];
/// A skill's extra files are capped so one catalog entry cannot plan thousands of writes.
const MAX_EXTRA_FILES: usize = 64;
/// A skill name must also be a usable directory name ([`check_segment`]).
const MAX_NAME_LEN: usize = 128;

/// One row of the catalog index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogSkill {
    /// Skill directory name (`skills/<group>/<name>/SKILL.md`).
    pub name: String,
    pub description: String,
    /// First path segment under `skills/`; empty when the entrypoint has no group.
    pub group: String,
    /// Raw URL of the `SKILL.md` body.
    pub entrypoint: String,
}

/// Parsed catalog index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    pub skills: Vec<CatalogSkill>,
}

/// One row of the repository file tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeEntry {
    /// Repository-relative path (`skills/<group>/<name>/references/java.md`).
    pub path: String,
    /// Blob id; changes whenever the file content changes.
    pub sha: String,
    pub is_blob: bool,
}

/// Parses the catalog index (`{ "skills": [ { name, description, entrypoint } ] }`).
///
/// Tolerant by design: an entry with an unusable name, a non-`http(s)` entrypoint or one that does not
/// point at a `.md` file is skipped instead of failing the whole catalog, so one bad upstream row cannot
/// break the page.
///
/// # Errors
///
/// The body is not valid UTF-8 JSON or has no `skills` array.
pub fn parse_catalog(bytes: &[u8]) -> Result<Catalog> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| ConfigError::invalid(format!("catalog index is not valid JSON: {e}")))?;
    let rows = value
        .get("skills")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ConfigError::invalid("catalog index has no 'skills' array"))?;
    let mut skills = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(name) = row.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(entrypoint) = row.get("entrypoint").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !usable_name(name) || !usable_entrypoint(entrypoint) {
            tracing::debug!(%name, %entrypoint, "catalog: skipping unusable entry");
            continue;
        }
        let description = row
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        skills.push(CatalogSkill {
            name: name.to_owned(),
            description,
            group: group_of(entrypoint).unwrap_or_default(),
            entrypoint: entrypoint.to_owned(),
        });
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills.dedup_by(|a, b| a.name == b.name);
    Ok(Catalog { skills })
}

/// Parses the repository file tree (`{ "tree": [ { path, type, sha } ] }`).
///
/// # Errors
///
/// The body is not valid UTF-8 JSON or has no `tree` array.
pub fn parse_tree(bytes: &[u8]) -> Result<Vec<TreeEntry>> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| ConfigError::invalid(format!("file tree is not valid JSON: {e}")))?;
    let rows = value
        .get("tree")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ConfigError::invalid("file tree has no 'tree' array"))?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            Some(TreeEntry {
                path: row.get("path")?.as_str()?.to_owned(),
                sha: row
                    .get("sha")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                is_blob: row.get("type").and_then(serde_json::Value::as_str) == Some("blob"),
            })
        })
        .collect())
}

/// Extra files of `skills/<group>/<name>/` as `(relative path, sha)`, `SKILL.md` excluded.
///
/// Paths that would escape the skill directory are dropped here; [`with_extra_files`] refuses them again
/// before planning, so a hostile tree cannot write outside the skill folder even if this filter is
/// bypassed.
#[must_use]
pub fn skill_files(tree: &[TreeEntry], group: &str, name: &str) -> Vec<(String, String)> {
    let prefix = format!("skills/{}/{name}/", group.trim_matches('/'));
    let mut files: Vec<(String, String)> = tree
        .iter()
        .filter(|e| e.is_blob)
        .filter_map(|e| {
            e.path
                .strip_prefix(prefix.as_str())
                .map(|rel| (rel, &e.sha))
        })
        .filter(|(rel, _)| !rel.is_empty() && *rel != SKILL_FILE)
        .filter(|(rel, _)| safe_relative(rel).is_ok())
        .map(|(rel, sha)| (rel.to_owned(), sha.clone()))
        .collect();
    files.sort();
    files.truncate(MAX_EXTRA_FILES);
    files
}

/// True when `rel` is a safe relative path inside a skill directory.
///
/// Rejects empty paths, absolute paths, Windows drive letters, backslashes, `.`/`..`/empty segments and
/// control characters, so `../../evil` can never become a target path.
pub fn safe_relative(rel: &str) -> Result<()> {
    let bad = |reason: &str| {
        Err(ConfigError::invalid(format!(
            "unsafe extra file {rel:?}: {reason}"
        )))
    };
    if rel.is_empty() {
        return bad("empty path");
    }
    if rel.starts_with('/') || rel.starts_with('\\') {
        return bad("absolute path");
    }
    if rel.contains('\\') {
        return bad("backslash separator");
    }
    if rel.len() > 1 && rel.as_bytes()[1] == b':' {
        return bad("drive letter");
    }
    if rel.chars().any(char::is_control) {
        return bad("control character");
    }
    for segment in rel.split('/') {
        if segment.is_empty() {
            return bad("empty segment");
        }
        if segment == "." || segment == ".." {
            return bad("parent segment");
        }
        if segment.len() > MAX_NAME_LEN {
            return bad("segment too long");
        }
    }
    Ok(())
}

/// A downloaded `SKILL.md` as a [`Skill`]: front matter aware, including block scalars.
///
/// `Doc::get` cannot express `description: >-` (it returns `">-"`), and the upstream catalog uses folded
/// block scalars for nearly every description, so the description is read from the raw entry here.
#[must_use]
pub fn parse_skill_md(name: &str, text: &str) -> Skill {
    let doc = Doc::parse(text);
    let description = doc
        .raw("description")
        .and_then(block_scalar)
        .filter(|d| !d.is_empty());
    Skill {
        name: name.to_owned(),
        description,
        body: doc.body.clone(),
        extra_front_matter: doc.extra(SKILL_KNOWN),
        files: Vec::new(),
        diagnostics: Vec::new(),
    }
}

/// Plans the install of `skill` into `dir` (`<dir>/<name>/SKILL.md` plus its extra files).
///
/// Reuses [`plan_skill_upsert`], so an install is a normal text write with a truthful before/after diff and
/// an existing file is updated instead of duplicated.
///
/// # Errors
///
/// Invalid skill name, unsafe `files` path, I/O while reading the current contents.
pub async fn plan_install(
    dir: &Path,
    skill: &Skill,
    files: &[(String, String)],
) -> Result<WritePlan> {
    let plan = plan_skill_upsert(dir, skill, None).await?;
    with_extra_files(plan, files).await
}

/// Appends one text change per extra file under the plan's skill directory.
///
/// `files` is `(relative path, file content)`. Each change reads the current content first so the diff
/// shows what really changes, and unsafe relative paths are refused instead of being joined.
///
/// # Errors
///
/// The plan has no `SKILL.md` write change, a path is unsafe, or reading the current file fails.
pub async fn with_extra_files(plan: WritePlan, files: &[(String, String)]) -> Result<WritePlan> {
    if files.is_empty() {
        return Ok(plan);
    }
    let dir = skill_dir_of(&plan).ok_or_else(|| {
        ConfigError::invalid(
            "install plan has no SKILL.md change to derive the skill directory from",
        )
    })?;
    if files.len() > MAX_EXTRA_FILES {
        return Err(ConfigError::invalid(format!(
            "skill has {} extra files; at most {MAX_EXTRA_FILES} are installed",
            files.len()
        )));
    }
    let mut plan = plan;
    for (rel, text) in files {
        safe_relative(rel)?;
        let path = rel.split('/').fold(dir.clone(), |p, seg| p.join(seg));
        let before = fsx::read_to_string_opt(&path).await?;
        plan.changes
            .push(FileChange::write(path, before, text.clone(), Kind::Text));
    }
    Ok(plan)
}

/// The directory holding the plan's `SKILL.md`.
fn skill_dir_of(plan: &WritePlan) -> Option<PathBuf> {
    plan.changes
        .iter()
        .find(|c| c.path.file_name().is_some_and(|n| n == SKILL_FILE))
        .and_then(|c| c.path.parent())
        .map(Path::to_path_buf)
}

/// Name usable as a skill directory.
fn usable_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME_LEN && check_segment(name).is_ok()
}

/// Entrypoint must be plain `http(s)` and served from a `.md` file.
fn usable_entrypoint(entrypoint: &str) -> bool {
    let lower = entrypoint.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && lower
            .split(['?', '#'])
            .next()
            .is_some_and(|p| p.to_ascii_lowercase().ends_with(".md"))
}

/// `skills/<group>/<name>/SKILL.md` → `group`.
fn group_of(entrypoint: &str) -> Option<String> {
    let path = entrypoint.split(['?', '#']).next()?;
    let segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    // .../skills/<group>/<name>/SKILL.md
    if segments.len() < 3 {
        return None;
    }
    let group = segments[segments.len() - 3];
    if segments[segments.len() - 2].is_empty() || group.is_empty() {
        return None;
    }
    Some(group.to_owned())
}

/// Text of a front matter value, folding block scalars (`>`, `>-`, `|`, `|-`) and plain continuation lines.
fn block_scalar(raw: &str) -> Option<String> {
    let mut lines = raw.split('\n');
    let header = lines.next().unwrap_or("").trim();
    let rest: Vec<&str> = lines.collect();
    if rest.is_empty() {
        let value = unquote(header);
        return (!value.is_empty()).then_some(value);
    }
    let literal = header.starts_with('|');
    let indent = rest
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let body: Vec<String> = rest
        .iter()
        .map(|l| l.get(indent..).unwrap_or("").trim_end().to_owned())
        .collect();
    let text = if literal {
        body.join("\n")
    } else {
        let mut out = String::new();
        for line in &body {
            if line.is_empty() {
                out.push('\n');
            } else {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push(' ');
                }
                out.push_str(line);
            }
        }
        out
    };
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
// Tests assert on fixtures; a panic is the failure message.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]
mod tests;
