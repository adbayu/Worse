use super::{
    js_trim, node_is_absolute, node_resolve, normalize_git_path, thread, ExecResult, File, HashMap,
    LineStat, Mutex, PoisonError, Read, StatusEntry, WatchPath, DEFAULT_GRAPH_MAX_COUNT,
    MAX_GRAPH_MAX_COUNT_F64, UNTRACKED_STAT_CHUNK_BYTES, UNTRACKED_STAT_CONCURRENCY,
    UNTRACKED_STAT_MAX_BYTES,
};

/// `normalizeWatchPath`.
pub(super) fn normalize_watch_path(path: &str) -> String {
    let trimmed = js_trim(path);
    let b = trimmed.as_bytes();
    let is_drive_root = (b.len() == 2 || b.len() == 3)
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'/' || b[2] == b'\\');
    if trimmed == "/" || is_drive_root {
        return trimmed.to_owned();
    }
    trimmed.trim_end_matches(['/', '\\']).to_owned()
}

pub(super) fn add_watch_path(paths: &mut Vec<WatchPath>, path: &str) {
    let normalized = normalize_watch_path(path);
    if normalized.is_empty() || paths.iter().any(|p| p.path == normalized) {
        return;
    }
    paths.push(WatchPath {
        path: normalized,
        recursive: true,
    });
}

/// `buildAutoRefreshWatchPaths`.
pub(super) fn build_watch_paths(
    workspace_path: &str,
    absolute_git_dir: &str,
    common_dir: &str,
) -> Vec<WatchPath> {
    let mut paths = Vec::new();
    add_watch_path(&mut paths, absolute_git_dir);
    let resolved_common = if common_dir.is_empty() {
        absolute_git_dir.to_owned()
    } else if node_is_absolute(common_dir) {
        common_dir.to_owned()
    } else {
        node_resolve(&[workspace_path, common_dir])
    };
    add_watch_path(&mut paths, &resolved_common);
    paths
}

pub(super) fn is_not_repository(result: &ExecResult) -> bool {
    let stderr = result.stderr.to_lowercase();
    stderr.contains("not a git repository") || stderr.contains("outside repository")
}

pub(super) fn is_missing_working_directory(result: &ExecResult) -> bool {
    let stderr = result.stderr.to_lowercase();
    (result.exit_code == Some(-2) && stderr.contains("enoent"))
        || stderr.contains("unable to read current working directory")
        || stderr.contains("no such file or directory")
}

/// `toAbsolutePath(repoRoot, repoRelativePath)`.
#[must_use]
pub fn to_absolute_path(repo_root: &str, repo_relative_path: &str) -> String {
    let normalized = normalize_git_path(repo_relative_path);
    let mut segments: Vec<&str> = vec![repo_root];
    segments.extend(normalized.split('/'));
    node_resolve(&segments)
}

/// Workspace-relative input (with `/`) resolved like
/// `resolve(workspacePath, path.split("/").join(sep))`.
#[must_use]
pub fn resolve_input_path(workspace_path: &str, path: &str) -> String {
    if node_is_absolute(path) {
        return path.to_owned();
    }
    let mut segments: Vec<&str> = vec![workspace_path];
    segments.extend(path.split('/'));
    node_resolve(&segments)
}

/// `countUntrackedFileLines` (errors count as 0 lines).
pub(super) fn count_untracked_lines(absolute_path: &str, buffer: &mut [u8]) -> u64 {
    let Ok(meta) = std::fs::metadata(absolute_path) else {
        return 0;
    };
    if !meta.is_file() || meta.len() > UNTRACKED_STAT_MAX_BYTES {
        return 0;
    }
    let Ok(mut file) = File::open(absolute_path) else {
        return 0;
    };
    let mut total: u64 = 0;
    let mut newlines: u64 = 0;
    let mut last_byte = b'\n';
    loop {
        let budget = usize::try_from(UNTRACKED_STAT_MAX_BYTES + 1 - total).unwrap_or(usize::MAX);
        let length = buffer.len().min(budget);
        let Ok(read) = file.read(&mut buffer[..length]) else {
            return 0;
        };
        if read == 0 {
            return newlines + u64::from(last_byte != b'\n');
        }
        total += read as u64;
        if total > UNTRACKED_STAT_MAX_BYTES {
            return 0;
        }
        for &b in &buffer[..read] {
            if b == 0 {
                return 0;
            }
            if b == b'\n' {
                newlines += 1;
            }
        }
        last_byte = buffer[read - 1];
    }
}

/// `buildUntrackedStats`.
pub(super) fn build_untracked_stats(
    repo_root: &str,
    entries: &[StatusEntry],
) -> HashMap<String, LineStat> {
    let untracked: Vec<&StatusEntry> = entries.iter().filter(|e| e.is_untracked).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Mutex<HashMap<String, LineStat>> = Mutex::new(HashMap::new());
    let workers = UNTRACKED_STAT_CONCURRENCY.min(untracked.len());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let mut buffer = vec![0u8; UNTRACKED_STAT_CHUNK_BYTES];
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(entry) = untracked.get(index) else {
                        break;
                    };
                    let absolute = to_absolute_path(repo_root, &entry.path);
                    let added = count_untracked_lines(&absolute, &mut buffer);
                    results
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(
                            entry.path.clone(),
                            LineStat {
                                added,
                                ..LineStat::default()
                            },
                        );
                }
            });
        }
    });
    results.into_inner().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn normalize_graph_max_count(value: Option<f64>) -> u64 {
    match value {
        Some(v) if v.is_finite() => {
            let floored = v.floor().clamp(1.0, MAX_GRAPH_MAX_COUNT_F64);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = floored as u64;
            n
        }
        _ => DEFAULT_GRAPH_MAX_COUNT,
    }
}

pub(super) fn normalize_graph_skip(value: Option<f64>) -> u64 {
    match value {
        Some(v) if v.is_finite() && v > 0.0 => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = v.floor() as u64;
            n
        }
        _ => 0,
    }
}
