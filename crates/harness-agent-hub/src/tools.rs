//! Harness-private tool installs under `<home>/tools/` (phase-9-parity.md §9.10).
//!
//! Owner: this module owns the tool directories. Claude Code, Codex and agy come from their official release
//! channels (the same manifests and checksums their install scripts use), Node.js from nodejs.org, and the npm ACP
//! adapters go to `<home>/tools/npm` (every npm call gets `npm_config_prefix`). Nothing is written outside
//! `<home>/tools/`: no PATH, shell profile or launcher of the user is touched. A tool already found on PATH wins;
//! these directories are searched after it ([`crate::setup::extra_search_dirs`]) and appended to the PATH of agent
//! and npm processes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod cli;
mod fetch;

pub(crate) use cli::{download_cli, download_node, downloadable};

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Sets `<home>/tools` and registers its bin directories for program lookup.
pub(crate) fn set_home(home: &Path) {
    let _ = ROOT.set(home.join("tools"));
    for dir in bin_dirs() {
        crate::setup::add_search_dir(dir);
    }
}

pub(crate) fn root() -> Option<PathBuf> {
    ROOT.get().cloned()
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// Directory of the Harness Node.js (`node`, `npm`).
fn node_bin() -> Option<PathBuf> {
    root().map(|r| {
        if cfg!(windows) {
            r.join("node")
        } else {
            r.join("node").join("bin")
        }
    })
}

/// npm prefix of the Harness adapters.
pub(crate) fn npm_prefix() -> Option<PathBuf> {
    root().map(|r| r.join("npm"))
}

fn npm_bin() -> Option<PathBuf> {
    npm_prefix().map(|p| if cfg!(windows) { p } else { p.join("bin") })
}

/// Bin directory of a downloaded CLI (`claude`, `codex`, `agy`).
fn cli_bin(cli: &str) -> Option<PathBuf> {
    root().map(|r| r.join(cli).join("bin"))
}

fn bin_dirs() -> Vec<PathBuf> {
    ["claude", "codex", "agy"]
        .iter()
        .filter_map(|cli| cli_bin(cli))
        .chain(root().map(|r| r.join("codex").join("codex-path")))
        .chain(node_bin())
        .chain(npm_bin())
        .collect()
}

/// npm of the Harness Node.js, when it was downloaded.
pub(crate) fn harness_npm() -> Option<PathBuf> {
    let name = if cfg!(windows) { "npm.cmd" } else { "npm" };
    node_bin().map(|d| d.join(name)).filter(|p| p.is_file())
}

/// PATH for agent and npm processes: the inherited PATH, then the Harness tool directories that exist.
/// `None` when no tool directory exists (the inherited PATH is kept as is).
pub(crate) fn path_env() -> Option<OsString> {
    let extra: Vec<PathBuf> = bin_dirs().into_iter().filter(|d| d.is_dir()).collect();
    if extra.is_empty() {
        return None;
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for dir in extra {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).ok()
}

/// Adds the Harness tool directories to the PATH of a spawned agent (`env` overrides the inherited environment).
pub(crate) fn with_path(env: &[(String, String)]) -> Vec<(String, String)> {
    let mut env = env.to_vec();
    if env.iter().any(|(k, _)| k.eq_ignore_ascii_case("PATH")) {
        return env;
    }
    if let Some(path) = path_env() {
        env.push(("PATH".to_owned(), path.to_string_lossy().into_owned()));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::with_path;

    #[test]
    fn an_explicit_path_is_kept() {
        let env = vec![("PATH".to_owned(), "x".to_owned())];
        assert_eq!(with_path(&env), env);
    }
}
