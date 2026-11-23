//! MEMORY.md files (spec §8.1). The one write path for the hub API and the MCP memory server.
//!
//! Synchronous on purpose: the MCP server binary has no async runtime, and the hub calls these through
//! `spawn_blocking`.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A lock older than this is considered abandoned by a crashed writer.
const STALE_LOCK: Duration = Duration::from_secs(30);
/// Wait step while another process holds the lock (cross-process contention, not a sync workaround).
const LOCK_STEP: Duration = Duration::from_millis(20);
const LOCK_ATTEMPTS: u32 = 250;

/// Memory scope resolved to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryTarget {
    Global,
    Project(PathBuf),
}

/// Resolves MEMORY.md paths.
#[derive(Debug, Clone)]
pub struct MemoryFiles {
    home: PathBuf,
}

impl MemoryFiles {
    /// `home` = Harness data directory.
    #[must_use]
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// File path of a scope.
    #[must_use]
    pub fn path(&self, target: &MemoryTarget) -> PathBuf {
        match target {
            MemoryTarget::Global => self.home.join("MEMORY.md"),
            MemoryTarget::Project(root) => root.join(".harness").join("MEMORY.md"),
        }
    }

    /// Reads a scope; missing file = empty.
    ///
    /// # Errors
    ///
    /// I/O errors other than not-found.
    pub fn read(&self, target: &MemoryTarget) -> std::io::Result<String> {
        match fs::read_to_string(self.path(target)) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e),
        }
    }

    /// Reads a scope; `None` when the file does not exist.
    ///
    /// # Errors
    ///
    /// I/O errors other than not-found.
    pub fn read_opt(&self, target: &MemoryTarget) -> std::io::Result<Option<String>> {
        match fs::read_to_string(self.path(target)) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Replaces the file only if its fingerprint still equals `base` (checked under the same lock as every other
    /// write). `Ok(false)` = conflict, nothing written (phase-9-parity.md §9.1.5a).
    ///
    /// # Errors
    ///
    /// I/O errors, or lock contention that does not clear.
    pub fn write_checked(
        &self,
        target: &MemoryTarget,
        base: &str,
        content: &str,
    ) -> std::io::Result<bool> {
        let path = self.path(target);
        let dir = path.parent().ok_or_else(|| {
            std::io::Error::new(ErrorKind::InvalidInput, "memory path has no parent")
        })?;
        fs::create_dir_all(dir)?;
        let lock = path.with_extension("md.lock");
        let _guard = LockGuard::acquire(&lock)?;
        let current = match fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if fingerprint(current.as_deref()) != base {
            return Ok(false);
        }
        replace_atomic(dir, &path, content)?;
        Ok(true)
    }

    /// Replaces or appends under an exclusive lock, then renames atomically.
    ///
    /// # Errors
    ///
    /// I/O errors, or lock contention that does not clear.
    pub fn write(&self, target: &MemoryTarget, content: &str, append: bool) -> std::io::Result<()> {
        let path = self.path(target);
        let dir = path.parent().ok_or_else(|| {
            std::io::Error::new(ErrorKind::InvalidInput, "memory path has no parent")
        })?;
        fs::create_dir_all(dir)?;
        let lock = path.with_extension("md.lock");
        let _guard = LockGuard::acquire(&lock)?;
        let mut next = if append {
            match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e),
            }
        } else {
            String::new()
        };
        if append && !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(content);
        replace_atomic(dir, &path, &next)
    }
}

fn replace_atomic(dir: &Path, path: &Path, content: &str) -> std::io::Result<()> {
    let tmp = dir.join(format!(".MEMORY.md.{}.tmp", std::process::id()));
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Fingerprint of a MEMORY.md content (`None` = missing file) for plan/apply conflict checks. Plans live only
/// inside one hub process, so the std hasher is sufficient.
#[must_use]
pub fn fingerprint(content: Option<&str>) -> String {
    use std::hash::{Hash, Hasher};
    match content {
        None => "absent".to_owned(),
        Some(text) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            text.hash(&mut hasher);
            format!("{}:{:016x}", text.len(), hasher.finish())
        }
    }
}

pub(crate) struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    pub(crate) fn acquire(path: &Path) -> std::io::Result<Self> {
        for _ in 0..LOCK_ATTEMPTS {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(_) => {
                    return Ok(Self {
                        path: path.to_path_buf(),
                    })
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok())
                        .is_some_and(|age| age > STALE_LOCK);
                    if stale {
                        let _ = fs::remove_file(path);
                        continue;
                    }
                    std::thread::sleep(LOCK_STEP);
                }
                // 修复（CI concurrent_writers_lose_nothing）：Windows 上另一个写入者刚删除锁文件、删除尚未
                // 完成时，create_new 返回 Access denied 而不是 AlreadyExists；当作“锁被占用”重试。
                Err(e) if cfg!(windows) && e.kind() == ErrorKind::PermissionDenied => {
                    std::thread::sleep(LOCK_STEP);
                }
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            ErrorKind::WouldBlock,
            "MEMORY.md is locked by another writer",
        ))
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The memory block injected at the start of every native agent session (spec §8.2).
#[must_use]
pub fn injection_block(global: &str, project: &str, project_root: &str) -> String {
    format!(
        "<harness-memory scope=\"global\">\n{}\n</harness-memory>\n<harness-memory scope=\"project\" path=\"{}\">\n{}\n</harness-memory>\nShared memory for all agents in this Harness session. Use the harness-memory MCP tools memory_read / memory_write (scope \"global\" or \"project\") to read or update it.",
        global.trim_end(),
        project_root,
        project.trim_end()
    )
}
