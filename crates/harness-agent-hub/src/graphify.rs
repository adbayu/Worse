//! Built-in support for Graphify (<https://github.com/Graphify-Labs/graphify>, Apache-2.0): a command-line
//! tool that turns a project into a knowledge graph an agent can query instead of grepping.
//!
//! Graphify is a Python package; Harness installs a pinned copy into `<HARNESS_HOME>/tools` (uv) and prefers it.
//! Harness only uses what it finds:
//!
//! - `<project>/graphify-out/graph.json` exists: a fresh agent session is told so, and how to ask the graph;
//! - `graphify-mcp` is installed too: its MCP server (stdio, `graphify-mcp <graph.json>`) is attached to the
//!   session, so every agent gets `query_graph`, `get_node`, `get_neighbors`, `shortest_path`, `god_nodes`…;
//! - Settings › Memory › Graph builds or refreshes the graph with `graphify update <project>`, the code-only
//!   pass (tree-sitter, no model, no API key).
//!
//! Nothing is written to `CLAUDE.md`, `AGENTS.md` or an agent's settings, unlike `graphify install`: the
//! guidance travels in Harness's own prompt block.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Output directory Graphify writes into the project.
pub const OUT_DIR: &str = "graphify-out";
/// The install command shown to the user (the `mcp` extra brings `graphify-mcp`'s dependencies).
pub const INSTALL_COMMAND: &str = "uv tool install \"graphifyy[mcp]==0.9.87\"";
/// Env overrides of the two programs (a custom location; tests use stand-ins). An override naming a missing
/// file means "not installed".
pub const ENV_CLI: &str = "HARNESS_GRAPHIFY";
pub const ENV_MCP: &str = "HARNESS_GRAPHIFY_MCP";
/// Name of the attached MCP server.
pub const MCP_NAME: &str = "graphify";

/// Files of a project's graph.
#[derive(Debug, Clone)]
pub struct Paths {
    pub graph: PathBuf,
    pub report: PathBuf,
    pub html: PathBuf,
}

/// Where Graphify keeps the graph of `workspace`.
#[must_use]
pub fn paths(workspace: &Path) -> Paths {
    let out = workspace.join(OUT_DIR);
    Paths {
        graph: out.join("graph.json"),
        report: out.join("GRAPH_REPORT.md"),
        html: out.join("graph.html"),
    }
}

/// Env override of the `uv` command (tests use a stand-in).
pub const ENV_UV: &str = "HARNESS_UV";

fn user_home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Directories where Python tool installers put launchers, beyond `PATH`: a shell started before the install,
/// or the Windows desktop session, often does not have them on `PATH`.
fn tool_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = user_home() {
        // `uv tool`, `pipx`, and `pip install --user` on Unix.
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".cargo").join("bin"));
    }
    if cfg!(windows) {
        // `pip install --user`: %APPDATA%\Python\Python3xx\Scripts.
        if let Some(roaming) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
            if let Ok(entries) = std::fs::read_dir(PathBuf::from(roaming).join("Python")) {
                dirs.extend(entries.flatten().map(|e| e.path().join("Scripts")));
            }
        }
        // winget's shims (uv installed with winget).
        if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            dirs.push(
                PathBuf::from(local)
                    .join("Microsoft")
                    .join("WinGet")
                    .join("Links"),
            );
        }
    }
    dirs
}

/// Version Harness installs into its own tool directory, so every computer runs the same Graphify.
pub const VERSION: &str = "0.9.87";
/// Package spec of the Harness-owned install.
pub const PACKAGE: &str = "graphifyy[mcp]==0.9.87";

/// Harness-owned uv tool directory (`UV_TOOL_DIR`): `<HARNESS_HOME>/tools/uv`.
#[must_use]
pub fn own_tool_dir() -> Option<PathBuf> {
    harness_config::harness_home().map(|h| h.join("tools").join("uv"))
}

/// Harness-owned launcher directory (`UV_TOOL_BIN_DIR`): `<HARNESS_HOME>/tools/bin`.
#[must_use]
pub fn own_bin_dir() -> Option<PathBuf> {
    harness_config::harness_home().map(|h| h.join("tools").join("bin"))
}

fn own_program(program: &str) -> Option<PathBuf> {
    let file = format!("{program}{}", std::env::consts::EXE_SUFFIX);
    own_bin_dir()
        .map(|dir| dir.join(file))
        .filter(|path| path.is_file())
}

/// Whether Harness's own pinned Graphify is installed.
#[must_use]
pub fn own_installed() -> bool {
    std::env::var_os(ENV_CLI).is_none() && own_program("graphify").is_some()
}

/// The Python interpreter of the environment `cli` runs in: the venv's `Scripts`/`bin` holds both, a plain
/// `pip install` puts the launcher in `<python>/Scripts`.
#[must_use]
pub fn python_of(cli: &Path) -> Option<PathBuf> {
    let file = format!("python{}", std::env::consts::EXE_SUFFIX);
    if let Some(dir) = own_tool_dir() {
        if own_program("graphify").as_deref() == Some(cli) {
            let scripts = if cfg!(windows) { "Scripts" } else { "bin" };
            let path = dir.join("graphifyy").join(scripts).join(&file);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    let here = cli.parent()?;
    [Some(here), here.parent()]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(&file))
        .find(|path| path.is_file())
}

/// Runs Graphify's command line with fixes for its crashes on hostile inputs: an encrypted or broken
/// `.docx`/`.xlsx` raised `RuntimeError` past the zip checks and aborted the whole build ("File … is
/// encrypted, password required for extraction"); such files are now skipped.
pub const SHIM: &str = r"import sys
import graphify.detect as _d
_caps = _d._zip_within_caps
def _safe_caps(path):
    try:
        return _caps(path)
    except Exception:
        return False
_d._zip_within_caps = _safe_caps
_conv = _d.convert_office_file
def _safe_conv(path, out_dir, root=None):
    try:
        return _conv(path, out_dir, root=root)
    except Exception:
        return None
_d.convert_office_file = _safe_conv
from graphify.__main__ import main
sys.argv = ['graphify'] + sys.argv[1:]
sys.exit(main())
";

fn find(env: &str, program: &str) -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os(env).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(custom);
        return path.is_file().then_some(path);
    }
    if let Some(own) = own_program(program) {
        return Some(own);
    }
    if let Some(found) = crate::agents::resolve_program(program) {
        return Some(found);
    }
    let file = format!("{program}{}", std::env::consts::EXE_SUFFIX);
    tool_dirs()
        .into_iter()
        .map(|dir| dir.join(&file))
        .find(|path| path.is_file())
}

/// The `uv` command (Astral's Python tool installer), if installed.
#[must_use]
pub fn uv() -> Option<PathBuf> {
    find(ENV_UV, "uv")
}

/// The `graphify` command, if installed.
#[must_use]
pub fn cli() -> Option<PathBuf> {
    find(ENV_CLI, "graphify")
}

/// The `graphify-mcp` command, if installed.
#[must_use]
pub fn mcp_cli() -> Option<PathBuf> {
    find(ENV_MCP, "graphify-mcp")
}

/// Size of a graph, for the settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub nodes: usize,
    pub links: usize,
    pub communities: usize,
}

/// Counts of a `graph.json` (`NetworkX` node-link data: `nodes`, and `links` or `edges`); `None` when the
/// text is not such a graph.
#[must_use]
pub fn stats(graph_json: &str) -> Option<Stats> {
    let graph: Value = serde_json::from_str(graph_json).ok()?;
    let nodes = graph.get("nodes")?.as_array()?;
    let links = graph
        .get("links")
        .or_else(|| graph.get("edges"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut communities: Vec<i64> = nodes
        .iter()
        .filter_map(|node| node.get("community").and_then(Value::as_i64))
        .collect();
    communities.sort_unstable();
    communities.dedup();
    Some(Stats {
        nodes: nodes.len(),
        links,
        communities: communities.len(),
    })
}

/// ACP `McpServer` (stdio) entry of the project's graph: only when the graph exists and `graphify-mcp` is
/// installed.
#[must_use]
pub fn mcp_server(workspace: &str) -> Option<Value> {
    let graph = paths(Path::new(workspace)).graph;
    if !graph.is_file() {
        return None;
    }
    let program = mcp_cli()?;
    Some(json!({
        "name": MCP_NAME,
        "command": program.to_string_lossy(),
        "args": [graph.to_string_lossy()],
        "env": []
    }))
}

/// What a fresh agent session is told about the project's graph; `None` when there is no graph.
#[must_use]
pub fn prompt_block(workspace: &str, now_ms: i64) -> Option<String> {
    let files = paths(Path::new(workspace));
    let built = std::fs::metadata(&files.graph)
        .ok()?
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    Some(block(
        built.map(|at| crate::recall::age(now_ms, at)).as_deref(),
        mcp_cli().is_some(),
        cli().is_some(),
        files.report.is_file(),
    ))
}

/// The text of [`prompt_block`] for what is available.
fn block(updated: Option<&str>, mcp: bool, cli: bool, report: bool) -> String {
    let mut text = format!(
        "<harness-graph>\nThis project has a Graphify knowledge graph ({OUT_DIR}/graph.json{}): its files and \
symbols and how they connect, grouped into communities.",
        updated.map_or_else(String::new, |age| format!(", updated {age}"))
    );
    let ask = match (mcp, cli) {
        (true, true) => " For a question about how the code fits together, ask the graph before you grep or \
read many files: the `graphify` MCP server (query_graph, get_node, get_neighbors, shortest_path, god_nodes, \
graph_stats), or `graphify query \"<question>\"` in a shell.",
        (true, false) => " For a question about how the code fits together, ask the graph before you grep or \
read many files: the `graphify` MCP server (query_graph, get_node, get_neighbors, shortest_path, god_nodes, \
graph_stats).",
        (false, true) => " For a question about how the code fits together, run `graphify query \
\"<question>\"` (also `graphify path A B`, `graphify explain <name>`) before you grep or read many files.",
        (false, false) => "",
    };
    text.push_str(ask);
    if report {
        use std::fmt::Write as _;
        let _ = write!(
            text,
            " {OUT_DIR}/GRAPH_REPORT.md is the overview (central nodes, communities); read it only for a \
broad architecture question."
        );
    }
    text.push_str(
        " The graph can be older than the code: check a file before you rely on a detail.",
    );
    if cli {
        text.push_str(
            " After you change code, `graphify update .` refreshes it (code only, no API cost).",
        );
    }
    text.push_str("\n</harness-graph>");
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn stats_count_nodes_links_and_communities() {
        let graph = r#"{"directed":false,"multigraph":false,"graph":{"schema_version":2},
            "nodes":[{"id":"client","label":"client.py","community":1},{"id":"models","community":1},{"id":"auth","community":4},{"id":"loose"}],
            "links":[{"source":"client","target":"models","relation":"imports_from","confidence":"EXTRACTED"},{"source":"client","target":"auth"}]}"#;
        assert_eq!(
            stats(graph),
            Some(Stats {
                nodes: 4,
                links: 2,
                communities: 2
            })
        );
        // Older files call the list `edges`.
        assert_eq!(
            stats(r#"{"nodes":[{"id":"a"}],"edges":[]}"#),
            Some(Stats {
                nodes: 1,
                links: 0,
                communities: 0
            })
        );
        assert_eq!(stats("{\"not\":\"a graph\"}"), None);
        assert_eq!(stats("garbage"), None);
    }

    #[test]
    fn the_block_names_only_what_is_installed() {
        let all = block(Some("2h ago"), true, true, true);
        assert!(all.starts_with("<harness-graph>\nThis project has a Graphify knowledge graph (graphify-out/graph.json, updated 2h ago)"));
        assert!(
            all.contains("`graphify` MCP server (query_graph")
                && all.contains("graphify query \"<question>\"")
        );
        assert!(all.contains("GRAPH_REPORT.md") && all.contains("graphify update ."));
        assert!(all.ends_with("</harness-graph>"));
        let mcp_only = block(None, true, false, false);
        assert!(
            mcp_only.contains("MCP server")
                && !mcp_only.contains("graphify query")
                && !mcp_only.contains("graphify update")
        );
        assert!(!mcp_only.contains("GRAPH_REPORT.md") && !mcp_only.contains("updated"));
        let cli_only = block(Some("3d ago"), false, true, true);
        assert!(!cli_only.contains("MCP server") && cli_only.contains("graphify path A B"));
        // A graph built elsewhere, nothing installed here: the files are still worth knowing about.
        let bare = block(Some("3d ago"), false, false, true);
        assert!(
            bare.contains("GRAPH_REPORT.md")
                && !bare.contains("graphify query")
                && !bare.contains("MCP")
        );
    }

    #[test]
    fn paths_sit_in_graphify_out() {
        let files = paths(Path::new("/work/demo"));
        assert_eq!(files.graph, Path::new("/work/demo/graphify-out/graph.json"));
        assert_eq!(
            files.report,
            Path::new("/work/demo/graphify-out/GRAPH_REPORT.md")
        );
        assert_eq!(files.html, Path::new("/work/demo/graphify-out/graph.html"));
    }
}
