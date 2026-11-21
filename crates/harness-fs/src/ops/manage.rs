use super::{canonicalize, fs, FileExists, FsError, Path};

/// Maximum number of paths per [`check_files_exist`] call (TS limit).
pub const FILE_EXISTENCE_BATCH_LIMIT: usize = 15;

/// Existence check for several paths, preserving input order. `exists` is
/// `stat(path).isFile()` and `false` on any error, as in the TS service.
///
/// # Errors
/// More than [`FILE_EXISTENCE_BATCH_LIMIT`] paths.
pub fn check_files_exist(paths: &[String]) -> Result<Vec<FileExists>, FsError> {
    if paths.len() > FILE_EXISTENCE_BATCH_LIMIT {
        return Err(FsError::app(format!(
            "File existence check supports at most {FILE_EXISTENCE_BATCH_LIMIT} paths."
        )));
    }
    Ok(paths
        .iter()
        .map(|path| FileExists {
            path: path.clone(),
            exists: fs::metadata(path).is_ok_and(|m| m.is_file()),
        })
        .collect())
}

/// `realpath` without the Windows verbatim prefix.
///
/// # Errors
/// Any OS error (e.g. `ENOENT`).
pub fn realpath(path: &str) -> Result<String, FsError> {
    Ok(canonicalize(Path::new(path))?
        .to_string_lossy()
        .into_owned())
}

/// Creates a directory. Returns `true` when something was created.
///
/// # Errors
/// Any OS error; with `recursive`, an existing directory is not an error.
pub fn mkdir(path: &str, recursive: bool) -> Result<bool, FsError> {
    let p = Path::new(path);
    if recursive {
        if p.is_dir() {
            return Ok(false);
        }
        fs::create_dir_all(p).map_err(|e| FsError::from_io(&e, "mkdir", p))?;
        return Ok(true);
    }
    fs::create_dir(p).map_err(|e| FsError::from_io(&e, "mkdir", p))?;
    Ok(true)
}

/// Removes a file, symlink or directory. Missing paths succeed when `force`.
///
/// # Errors
/// Any OS error; `ENOTEMPTY` for a non-empty directory without `recursive`.
pub fn remove(path: &str, recursive: bool, force: bool) -> Result<(), FsError> {
    let p = Path::new(path);
    let meta = match fs::symlink_metadata(p) {
        Ok(meta) => meta,
        Err(e) if force && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(FsError::from_io(&e, "unlink", p)),
    };
    let result = if meta.is_dir() {
        if recursive {
            fs::remove_dir_all(p)
        } else {
            fs::remove_dir(p)
        }
    } else {
        fs::remove_file(p).or_else(|e| {
            // Windows 目录软链接需要 remove_dir 删除。
            if cfg!(windows) && meta.file_type().is_symlink() {
                fs::remove_dir(p)
            } else {
                Err(e)
            }
        })
    };
    result.map_err(|e| FsError::from_io(&e, "unlink", p))
}

/// Renames `from` to `to`, replacing an existing file target.
///
/// # Errors
/// Any OS error.
pub fn rename(from: &str, to: &str) -> Result<(), FsError> {
    fs::rename(from, to).map_err(|e| FsError::from_io(&e, "rename", Path::new(from)))
}

/// Node `path.extname` on the last path segment (either separator).
pub(super) fn node_extname(path: &str) -> &str {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match base.rfind('.') {
        Some(index) if index > 0 && !base[..index].chars().all(|c| c == '.') => &base[index..],
        _ => "",
    }
}

/// Media type for a preview: the TS image table, then the shared
/// `MEDIA_PREVIEW_FORMATS` (audio/video), else `application/octet-stream`.
#[must_use]
pub fn media_type_for_path(path: &str) -> &'static str {
    const FORMATS: [(&str, &str); 11] = [
        (".mp4", "video/mp4"),
        (".mov", "video/quicktime"),
        (".webm", "video/webm"),
        (".m4v", "video/x-m4v"),
        (".mp3", "audio/mpeg"),
        (".wav", "audio/wav"),
        (".m4a", "audio/mp4"),
        (".ogg", "audio/ogg"),
        (".opus", "audio/opus"),
        (".flac", "audio/flac"),
        (".weba", "audio/webm"),
    ];
    let ext = node_extname(path).to_lowercase();
    let image = match ext.as_str() {
        ".apng" => Some("image/apng"),
        ".avif" => Some("image/avif"),
        ".bmp" => Some("image/bmp"),
        ".gif" => Some("image/gif"),
        ".ico" => Some("image/x-icon"),
        ".jpeg" | ".jpg" => Some("image/jpeg"),
        ".png" => Some("image/png"),
        ".svg" => Some("image/svg+xml"),
        ".webp" => Some("image/webp"),
        _ => None,
    };
    if let Some(media) = image {
        return media;
    }
    let normalized = path.replace('\\', "/").to_lowercase();
    FORMATS
        .iter()
        .find(|(extension, _)| normalized.ends_with(extension))
        .map_or("application/octet-stream", |(_, media)| media)
}
