use super::{
    fsx, AsyncReadExt, Cleanup, Component, FailPoint, Path, PathBuf, Serialize, SettingsError,
    CLEANUP_FILE, CONTROL_FILES, COPY_MARKER,
};

pub(super) fn interrupt(fail_at: Option<FailPoint>, here: FailPoint) -> Result<(), SettingsError> {
    if fail_at == Some(here) {
        return Err(relocation_error(format!(
            "interrupted at {here:?} (test hook)"
        )));
    }
    Ok(())
}

pub(super) async fn validate(
    base: &Path,
    current: &Path,
    target: &Path,
) -> Result<(), SettingsError> {
    if !target.is_absolute() {
        return Err(relocation_error("target must be an absolute path"));
    }
    let target = absolute(target)?;
    let current = absolute(current)?;
    if same_path(&target, &current) {
        return Err(relocation_error("target is the current data dir"));
    }
    if is_within(&target, &current) || is_within(&current, &target) {
        return Err(relocation_error(
            "target and current data dir must not contain each other",
        ));
    }
    if same_path(&target, base) {
        // Moving back to the base dir: it may only hold the relocation control files.
        let foreign = dir_entries(&target)
            .await?
            .into_iter()
            .filter(|name| !CONTROL_FILES.contains(&name.as_str()))
            .count();
        if foreign > 0 {
            return Err(relocation_error("the base dir is not empty"));
        }
        return Ok(());
    }
    let entries = dir_entries(&target).await?;
    if !entries.is_empty() && !entries.iter().any(|n| n == COPY_MARKER) {
        return Err(relocation_error("target directory is not empty"));
    }
    Ok(())
}

/// Deletes old files listed in the cleanup manifest (unless they are the data dir in use).
pub(super) async fn resume_cleanup(base: &Path, in_use: &Path) -> Result<(), SettingsError> {
    let manifest = base.join(CLEANUP_FILE);
    let Some(cleanup) = read_json::<Cleanup>(&manifest).await? else {
        return Ok(());
    };
    if same_path(&cleanup.from, in_use) {
        // 指针切换未生效（例如目标标记丢失而回退到旧目录）时，旧目录仍在使用，绝不能按清单删除。
        tracing::warn!("cleanup manifest names the data dir in use; dropped");
        return Ok(fsx::remove_file_opt(&manifest).await?);
    }
    for rel in &cleanup.files {
        if !is_plain_relative(rel) {
            continue;
        }
        let path = cleanup.from.join(rel);
        match tokio::fs::remove_file(&path).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    remove_empty_dirs(&cleanup.from, cleanup.remove_dir).await;
    Ok(fsx::remove_file_opt(&manifest).await?)
}

/// Removes empty sub-directories (deepest first) and, when `include_root`, the root if it ends up empty.
pub(super) async fn remove_empty_dirs(root: &Path, include_root: bool) {
    let mut dirs = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            if entry.file_type().await.is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
                dirs.push(entry.path());
            }
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for dir in dirs {
        let _ = tokio::fs::remove_dir(&dir).await;
    }
    if include_root {
        let _ = tokio::fs::remove_dir(root).await;
    }
}

/// Relative paths of all files to copy, and credential-like files that are skipped.
pub(super) async fn list_files(root: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>), SettingsError> {
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel_dir) = stack.pop() {
        let mut rd = tokio::fs::read_dir(root.join(&rel_dir)).await?;
        while let Some(entry) = rd.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = rel_dir.join(&name);
            let at_root = rel_dir.as_os_str().is_empty();
            if at_root && CONTROL_FILES.contains(&name.as_str()) {
                continue;
            }
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                if at_root && name == "tmp" {
                    continue;
                }
                stack.push(rel);
            } else if rel
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("lock"))
                || fsx::is_temp_file(&rel)
            {
                // 锁文件和临时文件不属于数据：既不复制也不删除。
            } else if fsx::is_forbidden(&rel) {
                skipped.push(rel);
            } else {
                files.push(rel);
            }
        }
    }
    files.sort();
    Ok((files, skipped))
}

pub(super) async fn same_bytes(a: &Path, b: &Path) -> Result<bool, SettingsError> {
    let (mut fa, mut fb) = (
        tokio::fs::File::open(a).await?,
        tokio::fs::File::open(b).await?,
    );
    if fa.metadata().await?.len() != fb.metadata().await?.len() {
        return Ok(false);
    }
    let mut ba = vec![0u8; 64 * 1024];
    let mut bb = vec![0u8; 64 * 1024];
    loop {
        let na = read_full(&mut fa, &mut ba).await?;
        let nb = read_full(&mut fb, &mut bb).await?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

pub(super) async fn read_full(
    file: &mut tokio::fs::File,
    buf: &mut [u8],
) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

pub(super) async fn clear_dir(dir: &Path) -> Result<(), SettingsError> {
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        if entry.file_type().await?.is_dir() {
            tokio::fs::remove_dir_all(entry.path()).await?;
        } else {
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    Ok(())
}

pub(super) async fn dir_entries(dir: &Path) -> Result<Vec<String>, SettingsError> {
    let mut names = Vec::new();
    match tokio::fs::read_dir(dir).await {
        Ok(mut rd) => {
            while let Some(entry) = rd.next_entry().await? {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(names)
}

pub(super) fn absolute(path: &Path) -> Result<PathBuf, SettingsError> {
    let abs = std::path::absolute(path)?;
    // Lexical normalisation (no symlink resolution: the target may not exist yet).
    let mut out = PathBuf::new();
    for component in abs.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

pub(super) fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/').to_owned();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

pub(super) fn same_path(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}

/// True when `inner` is strictly inside `outer`.
pub(super) fn is_within(inner: &Path, outer: &Path) -> bool {
    let (inner, outer) = (path_key(inner), path_key(outer));
    inner.len() > outer.len() && inner.starts_with(&outer) && inner[outer.len()..].starts_with('/')
}

pub(super) fn is_plain_relative(path: &Path) -> bool {
    path.components().all(|c| matches!(c, Component::Normal(_)))
}

pub(super) fn relocation_error(message: impl Into<String>) -> SettingsError {
    SettingsError::Relocation(message.into())
}

pub(super) async fn read_json<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, SettingsError> {
    let Some(text) = fsx::read_to_string_opt(path).await? else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| SettingsError::Invalid {
            path: path.display().to_string(),
            message: e.to_string(),
        })
}

pub(super) async fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), SettingsError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| SettingsError::Invalid {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(fsx::write_atomic(path, text.as_bytes()).await?)
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
