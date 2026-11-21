//! Markdown definitions in directories: skills (`<dir>/<name>/SKILL.md`), commands (`<dir>/**/<name>.md`, `a:b`
//! for `a/b.md`) and subagents (`<dir>/<name>.md`).

use std::path::{Path, PathBuf};

use harness_settings::fsx;

use crate::error::{ConfigError, Result};
use crate::frontmatter::Doc;
use crate::plan::{FileChange, Kind, WritePlan};
use crate::types::{Command, Skill, Subagent};

mod subagents;

pub(crate) use subagents::{list_subagents, plan_subagent_delete, plan_subagent_upsert};

const SKILL_FILE: &str = "SKILL.md";
const SKILL_KNOWN: &[&str] = &["name", "description"];
const COMMAND_KNOWN: &[&str] = &["description", "argument-hint", "model", "allowed-tools"];
const AGENT_KNOWN: &[&str] = &["name", "description", "tools", "model", "color"];

/// A safe single path segment (no separators, no leading dot, not empty).
pub(crate) fn check_segment(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        && name.len() <= 128;
    if ok {
        Ok(())
    } else {
        Err(ConfigError::invalid(format!("invalid name {name:?}")))
    }
}

/// Reads a listed definition file. `Ok(Some((text, true)))` when it was not UTF-8 and had to be decoded.
///
/// 修复：Settings › Skills 显示 "io: invalid utf-8 sequence of 1 bytes from index 337"。原因是用户的某个
/// SKILL.md 以 Windows-1252 保存（0x97 = em dash），严格 UTF-8 读取失败后整个列表都报错。列表只用于展示，
/// 因此单个文件编码不对时按 BOM 解码 UTF-16，否则按 Windows-1252 解码并继续；写入路径仍保持严格 UTF-8，
/// 避免静默改写文件内容。
async fn read_listed(path: &Path) -> Result<Option<(String, bool)>> {
    match fsx::read_to_string_opt(path).await {
        Ok(text) => Ok(text.map(|t| {
            let text = t.strip_prefix('\u{feff}').map(str::to_owned).unwrap_or(t);
            (text, false)
        })),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            // fsx 的禁止路径检查已经通过（否则错误类型不是 InvalidData），这里只重新取字节。
            let bytes = tokio::fs::read(path).await?;
            Ok(Some((decode_non_utf8(&bytes), true)))
        }
        Err(e) => Err(e.into()),
    }
}

/// UTF-16 (with BOM) or Windows-1252 text as a `String`.
fn decode_non_utf8(bytes: &[u8]) -> String {
    let utf16 = |rest: &[u8], le: bool| {
        let units = rest.chunks_exact(2).map(|p| {
            if le {
                u16::from_le_bytes([p[0], p[1]])
            } else {
                u16::from_be_bytes([p[0], p[1]])
            }
        });
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        _ => bytes.iter().map(|&b| windows_1252(b)).collect(),
    }
}

fn windows_1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}

async fn read_dir_sorted(dir: &Path) -> Result<Vec<(String, bool)>> {
    let mut out = Vec::new();
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().await?.is_dir();
        out.push((name, is_dir));
    }
    out.sort();
    Ok(out)
}

// ----------------------------------------------------------------------------------------------- skills

pub(crate) async fn list_skills(dir: &Path) -> Result<Vec<(Skill, PathBuf)>> {
    let mut out = Vec::new();
    for (name, is_dir) in read_dir_sorted(dir).await? {
        if !is_dir || name.starts_with('.') {
            continue;
        }
        let path = dir.join(&name).join(SKILL_FILE);
        let Some((text, not_utf8)) = read_listed(&path).await? else {
            continue;
        };
        let doc = Doc::parse(&text);
        let mut diagnostics = Vec::new();
        if not_utf8 {
            diagnostics.push(
                "SKILL.md is not UTF-8 (shown decoded); save it as UTF-8 to edit it here"
                    .to_owned(),
            );
        }
        let description = doc.get("description");
        if description.is_none() {
            diagnostics.push("missing description".to_owned());
        }
        if let Some(fm_name) = doc.get("name").filter(|n| *n != name) {
            diagnostics.push(format!(
                "front matter name {fm_name:?} differs from folder {name:?}"
            ));
        }
        let files = read_dir_sorted(&dir.join(&name))
            .await?
            .into_iter()
            .filter(|(f, _)| f != SKILL_FILE)
            .map(|(f, d)| if d { format!("{f}/") } else { f })
            .collect();
        out.push((
            Skill {
                name,
                description,
                extra_front_matter: doc.extra(SKILL_KNOWN),
                body: doc.body,
                files,
                diagnostics,
            },
            path,
        ));
    }
    Ok(out)
}

pub(crate) async fn plan_skill_upsert(
    dir: &Path,
    skill: &Skill,
    previous_name: Option<&str>,
) -> Result<WritePlan> {
    check_segment(&skill.name)?;
    if previous_name.is_some_and(|p| p != skill.name) {
        return Err(ConfigError::invalid(
            "renaming a skill is not supported: create the new skill, then delete the old one",
        ));
    }
    let path = dir.join(&skill.name).join(SKILL_FILE);
    let before = fsx::read_to_string_opt(&path).await?;
    let mut doc = before.as_deref().map(Doc::parse).unwrap_or_default();
    doc.set("name", Some(&skill.name));
    doc.set("description", skill.description.as_deref());
    doc.set_extra(SKILL_KNOWN, &skill.extra_front_matter);
    doc.body.clone_from(&skill.body);
    Ok(WritePlan::single(FileChange::write(
        path,
        before,
        doc.render(),
        Kind::Text,
    )))
}

pub(crate) async fn plan_skill_delete(dir: &Path, name: &str) -> Result<WritePlan> {
    check_segment(name)?;
    let skill_dir = dir.join(name);
    let guard = skill_dir.join(SKILL_FILE);
    let before = fsx::read_to_string_opt(&guard).await?;
    if before.is_none() {
        return Err(ConfigError::NotFound(name.to_owned()));
    }
    Ok(WritePlan::single(FileChange::remove_dir(
        skill_dir, guard, before,
    )))
}

// ----------------------------------------------------------------------------------------------- commands

fn command_path(dir: &Path, name: &str, nested: bool) -> Result<PathBuf> {
    let parts: Vec<&str> = name.split(':').collect();
    if !nested && parts.len() > 1 {
        return Err(ConfigError::invalid(
            "this CLI has no command folders (no ':' in names)",
        ));
    }
    let mut path = dir.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        check_segment(part)?;
        if i + 1 == parts.len() {
            path.push(format!("{part}.md"));
        } else {
            path.push(part);
        }
    }
    Ok(path)
}

pub(crate) async fn list_commands(dir: &Path, nested: bool) -> Result<Vec<(Command, PathBuf)>> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(dir.to_path_buf(), String::new())];
    while let Some((current, prefix)) = stack.pop() {
        for (name, is_dir) in read_dir_sorted(&current).await? {
            if name.starts_with('.') {
                continue;
            }
            if is_dir {
                if nested {
                    stack.push((current.join(&name), format!("{prefix}{name}:")));
                }
                continue;
            }
            let Some(stem) = name.strip_suffix(".md") else {
                continue;
            };
            let path = current.join(&name);
            let Some((text, _)) = read_listed(&path).await? else {
                continue;
            };
            let doc = Doc::parse(&text);
            out.push((
                Command {
                    name: format!("{prefix}{stem}"),
                    description: doc.get("description"),
                    argument_hint: doc.get("argument-hint"),
                    model: doc.get("model"),
                    allowed_tools: doc.get("allowed-tools"),
                    extra_front_matter: doc.extra(COMMAND_KNOWN),
                    body: doc.body,
                },
                path,
            ));
        }
    }
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    Ok(out)
}

pub(crate) async fn plan_command_upsert(
    dir: &Path,
    nested: bool,
    command: &Command,
    previous_name: Option<&str>,
) -> Result<WritePlan> {
    let path = command_path(dir, &command.name, nested)?;
    let mut plan = WritePlan::default();
    let mut source = path.clone();
    if let Some(previous) = previous_name.filter(|p| *p != command.name) {
        if fsx::exists(&path).await {
            return Err(ConfigError::invalid(format!(
                "a command named {} already exists",
                command.name
            )));
        }
        source = command_path(dir, previous, nested)?;
        let old = fsx::read_to_string_opt(&source).await?;
        if old.is_none() {
            return Err(ConfigError::NotFound(previous.to_owned()));
        }
        plan.changes
            .push(FileChange::delete(source.clone(), old, Kind::Text));
    }
    let base = fsx::read_to_string_opt(&source).await?;
    let mut doc = base.as_deref().map(Doc::parse).unwrap_or_default();
    doc.set("description", command.description.as_deref());
    doc.set("argument-hint", command.argument_hint.as_deref());
    doc.set("model", command.model.as_deref());
    doc.set("allowed-tools", command.allowed_tools.as_deref());
    doc.set_extra(COMMAND_KNOWN, &command.extra_front_matter);
    doc.body.clone_from(&command.body);
    let before = if source == path { base } else { None };
    plan.changes
        .insert(0, FileChange::write(path, before, doc.render(), Kind::Text));
    Ok(plan)
}

pub(crate) async fn plan_command_delete(dir: &Path, nested: bool, name: &str) -> Result<WritePlan> {
    let path = command_path(dir, name, nested)?;
    let before = fsx::read_to_string_opt(&path).await?;
    if before.is_none() {
        return Err(ConfigError::NotFound(name.to_owned()));
    }
    Ok(WritePlan::single(FileChange::delete(
        path,
        before,
        Kind::Text,
    )))
}
