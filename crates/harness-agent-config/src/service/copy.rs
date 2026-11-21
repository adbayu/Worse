use super::{fsx, FileChange, Kind, McpServer, Path, PathBuf, Result};

pub(super) const SKILL_COPY_MAX_FILES: usize = 200;
pub(super) const SKILL_COPY_MAX_BYTES: u64 = 1024 * 1024;

pub(super) async fn skill_file_copies(source: &Path, target: &Path) -> Result<Vec<FileChange>> {
    let mut changes = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let mut dir = match tokio::fs::read_dir(source.join(&relative)).await {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let mut names = Vec::new();
        while let Some(item) = dir.next_entry().await? {
            names.push((item.file_name(), item.file_type().await?));
        }
        names.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, kind) in names {
            let child = relative.join(&name);
            let hidden = name.to_string_lossy().starts_with('.');
            if kind.is_dir() {
                if !hidden {
                    pending.push(child);
                }
                continue;
            }
            if !kind.is_file() || child.as_os_str() == "SKILL.md" {
                continue;
            }
            if changes.len() >= SKILL_COPY_MAX_FILES {
                return Ok(changes);
            }
            let from = source.join(&child);
            if tokio::fs::metadata(&from).await?.len() > SKILL_COPY_MAX_BYTES {
                continue;
            }
            let Ok(text) = String::from_utf8(tokio::fs::read(&from).await?) else {
                continue;
            };
            let to = target.join(&child);
            let before = fsx::read_to_string_opt(&to).await?;
            changes.push(FileChange::write(to, before, text, Kind::Text));
        }
    }
    Ok(changes)
}

pub(super) fn mcp_summary(s: &McpServer) -> String {
    match (&s.command, &s.url) {
        (Some(cmd), _) if s.url.is_none() => std::iter::once(cmd.as_str())
            .chain(s.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        (_, Some(url)) => url.clone(),
        _ => String::new(),
    }
}
