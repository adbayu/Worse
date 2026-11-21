use super::{
    fs, inspect, report, sniff, ContentKind, ContentReport, HashMap, Mutex, Path, PathBuf,
    SystemTime, VecDeque, CACHE_CAPACITY,
};

pub(super) struct CacheEntry {
    mtime: Option<SystemTime>,
    len: u64,
    kind: ContentKind,
}

/// Path+mtime cache for [`ContentCache::inspect`], bounded and LRU.
pub struct ContentCache {
    entries: Mutex<(HashMap<PathBuf, CacheEntry>, VecDeque<PathBuf>)>,
}

impl Default for ContentCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentCache {
    /// Empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Mutex::new((HashMap::new(), VecDeque::new())),
        }
    }

    /// Same report as [`inspect`], reusing the last result while `(mtime, len)` is unchanged.
    pub fn inspect(&self, path: &Path) -> ContentReport {
        let meta = fs::metadata(path).ok();
        let stamp = meta.as_ref().map(|m| m.modified().ok());
        let len = meta.as_ref().map_or(0, fs::Metadata::len);
        let hit = {
            let Ok(guard) = self.entries.lock() else {
                return inspect(path);
            };
            guard
                .0
                .get(path)
                .filter(|e| e.len == len && e.mtime == stamp.flatten())
                .map(|e| e.kind)
        };
        let kind = hit.unwrap_or_else(|| sniff(path));
        if let Ok(mut guard) = self.entries.lock() {
            guard.0.insert(
                path.to_path_buf(),
                CacheEntry {
                    mtime: stamp.flatten(),
                    len,
                    kind,
                },
            );
            guard.1.retain(|p| p != path);
            guard.1.push_back(path.to_path_buf());
            while guard.1.len() > CACHE_CAPACITY {
                if let Some(old) = guard.1.pop_front() {
                    guard.0.remove(&old);
                }
            }
        }
        report(path, kind)
    }

    /// Number of cached paths; the cache is bounded by [`CACHE_CAPACITY`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().map_or(0, |g| g.0.len())
    }

    /// Whether nothing is cached yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
