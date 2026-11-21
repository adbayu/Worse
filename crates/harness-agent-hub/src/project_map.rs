use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

mod scan;

use scan::{build, GraphSummary, Tree};

const WALK_DEADLINE: Duration = Duration::from_millis(1500);
const WALK_FILES_MAX: usize = 60_000;
const WALK_DEPTH: usize = 6;
const DIRS_SHOWN: usize = 12;
const SUBDIRS_SHOWN: usize = 6;
const ENTRIES_SHOWN: usize = 14;
const HOT_SHOWN: usize = 10;
const GOD_SHOWN: usize = 10;
const COMMUNITIES_SHOWN: usize = 8;
const GRAPH_BYTES_MAX: u64 = 64 * 1024 * 1024;
const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".git",
    ".next",
    ".nuxt",
    ".turbo",
    ".gradle",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    "coverage",
    "graphify-out",
];
const ENTRY_FILES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "main.rs",
    "lib.rs",
    "main.go",
    "main.py",
    "__main__.py",
    "app.py",
    "manage.py",
    "index.ts",
    "index.tsx",
    "index.js",
    "main.ts",
    "main.tsx",
    "main.js",
    "server.ts",
    "server.js",
    "App.tsx",
    "Program.cs",
    "Main.java",
    "main.kt",
    "main.c",
    "main.cpp",
];

static BUILDING: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

#[must_use]
pub fn prompt_block(home: &Path, workspace: &str) -> Option<String> {
    let root = PathBuf::from(workspace);
    if !root.is_dir() || !is_project(&root) {
        return None;
    }
    let stamp = stamp(&root);
    let cache = cache_path(home, workspace);
    match read_cache(&cache) {
        Some((cached, body)) if cached == stamp => non_empty(body),
        Some((_, body)) => {
            rebuild_later(root, &cache, stamp);
            non_empty(body)
        }
        None => {
            let body = build(&root);
            write_cache(&cache, &stamp, &body);
            non_empty(body)
        }
    }
}

fn non_empty(body: String) -> Option<String> {
    (!body.trim().is_empty()).then_some(body)
}

fn is_project(root: &Path) -> bool {
    if root.parent().is_none() {
        return false;
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    home.map_or(true, |home| !same_path(&home, root))
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| crate::recall::project_key(&p.to_string_lossy());
    norm(a) == norm(b)
}

fn cache_path(home: &Path, workspace: &str) -> PathBuf {
    let key = crate::recall::project_key(workspace);
    let digest = Sha256::digest(key.as_bytes());
    let name: String = digest.iter().take(8).fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    home.join("memory").join("maps").join(format!("{name}.md"))
}

fn read_cache(path: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let (first, body) = text.split_once('\n')?;
    let stamp = first.strip_prefix("stamp: ")?;
    Some((stamp.to_owned(), body.to_owned()))
}

fn write_cache(path: &Path, stamp: &str, body: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, format!("stamp: {stamp}\n{body}")).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn rebuild_later(root: PathBuf, cache: &Path, stamp: String) {
    {
        let Ok(mut guard) = BUILDING.lock() else {
            return;
        };
        let set = guard.get_or_insert_with(HashSet::new);
        if !set.insert(cache.to_path_buf()) {
            return;
        }
    }
    let spawned = std::thread::Builder::new()
        .name("harness-project-map".into())
        .spawn({
            let cache = cache.to_path_buf();
            move || {
                let body = build(&root);
                write_cache(&cache, &stamp, &body);
                done(&cache);
            }
        });
    if spawned.is_err() {
        done(cache);
    }
}

fn done(cache: &Path) {
    if let Ok(mut guard) = BUILDING.lock() {
        if let Some(set) = guard.as_mut() {
            set.remove(cache);
        }
    }
}

fn stamp(root: &Path) -> String {
    let head = git_head(root).unwrap_or_else(|| "nogit".to_owned());
    let graph = std::fs::metadata(crate::graphify::paths(root).graph)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    format!("{head} {graph}")
}

fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    let path = PathBuf::from(target);
    Some(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

fn git_head(root: &Path) -> Option<String> {
    let dir = git_dir(root)?;
    let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        return Some(head.to_owned());
    };
    let common = std::fs::read_to_string(dir.join("commondir"))
        .ok()
        .map_or_else(|| dir.clone(), |c| dir.join(c.trim()));
    for base in [&dir, &common] {
        if let Ok(sha) = std::fs::read_to_string(base.join(reference)) {
            return Some(sha.trim().to_owned());
        }
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok();
    packed
        .as_deref()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_suffix(reference)?.trim().split(' ').next())
                .map(str::to_owned)
        })
        .or_else(|| Some(reference.to_owned()))
}

fn render(
    root: &Path,
    tree: &Tree,
    hot: &[(String, usize)],
    graph: Option<&GraphSummary>,
) -> String {
    if tree.dirs.is_empty() && tree.entries.is_empty() && graph.is_none() {
        return String::new();
    }
    let name = root
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let mut text = format!(
        "<harness-map>\nMap of {name}{}: a starting point to search from, not a replacement for reading the \
files you change.\n",
        if graph.is_some() {
            " (file tree, git history and the Graphify graph)"
        } else {
            " (file tree and git history)"
        }
    );
    let mut dirs: Vec<(&String, &usize)> = tree.dirs.iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    if !dirs.is_empty() {
        let _ = writeln!(
            text,
            "Folders (files{}):",
            if tree.partial { ", counted partly" } else { "" }
        );
        for (dir, count) in dirs.iter().take(DIRS_SHOWN) {
            let _ = write!(text, "- {dir}/ {count}");
            if let Some(subdirs) = tree.subdirs.get(*dir) {
                let mut subdirs: Vec<(&String, &usize)> = subdirs.iter().collect();
                subdirs.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
                let names: Vec<&str> = subdirs
                    .iter()
                    .take(SUBDIRS_SHOWN)
                    .map(|(n, _)| n.as_str())
                    .collect();
                if !names.is_empty() {
                    let more = if subdirs.len() > SUBDIRS_SHOWN {
                        ", …"
                    } else {
                        ""
                    };
                    let _ = write!(text, ": {}{more}", names.join(", "));
                }
            }
            text.push('\n');
        }
        if dirs.len() > DIRS_SHOWN {
            let _ = writeln!(text, "- … {} more folders", dirs.len() - DIRS_SHOWN);
        }
    }
    if !tree.entries.is_empty() {
        let shown: Vec<&str> = tree
            .entries
            .iter()
            .take(ENTRIES_SHOWN)
            .map(String::as_str)
            .collect();
        let _ = writeln!(text, "Entry points and manifests: {}", shown.join(", "));
    }
    if !hot.is_empty() {
        let shown: Vec<String> = hot.iter().map(|(p, n)| format!("{p} ({n})")).collect();
        let _ = writeln!(
            text,
            "Changed most in the last 300 commits: {}",
            shown.join(", ")
        );
    }
    if let Some(graph) = graph {
        if !graph.god.is_empty() {
            let shown: Vec<String> = graph
                .god
                .iter()
                .map(|(label, file, degree)| {
                    if file.is_empty() {
                        format!("{label} ({degree})")
                    } else {
                        format!("{label} [{file}] ({degree})")
                    }
                })
                .collect();
            let _ = writeln!(text, "Most connected symbols: {}", shown.join(", "));
        }
        if !graph.communities.is_empty() {
            text.push_str("Communities (size: main members, folder):\n");
            for (size, names, folder) in &graph.communities {
                let _ = write!(text, "- {size}: {}", names.join(", "));
                if !folder.is_empty() {
                    let _ = write!(text, " ({folder})");
                }
                text.push('\n');
            }
        }
    }
    text.push_str("</harness-map>");
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
