//! First-run setup: find and install missing ACP adapters (phase-9-parity.md §9.10).
//!
//! Owner: this module owns the table of known adapters, the process-wide adapter search directories (read by
//! [`crate::agents::resolve_program`]) and the single-flight set of running installs. Missing CLIs Harness can
//! download (Claude Code, Codex, agy) and Node.js come from their official channels into `<home>/tools` ([`crate::tools`]);
//! adapters are installed with `npm install -g <package>` under the Harness npm prefix. Probing never runs a program.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use harness_core_api::{
    AdapterInstallResult, AdapterInstallStatus, AgentId, ApiError, ApiResult, Availability,
    EventKind, GhSetup, SetupAgent, SetupState, SetupStatus,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::agents::{resolve_program, AgentSpec};
use crate::hub::Hub;

mod download;
mod install;

/// An agent CLI Harness knows, with the npm package of its ACP adapter (`None`: the CLI speaks ACP itself).
#[derive(Debug, Clone, Copy)]
pub struct KnownCli {
    /// Launch program of the agent (the key; Claude accounts share the Claude row).
    pub program: &'static str,
    /// CLI command name.
    pub cli: &'static str,
    /// npm package of the ACP adapter.
    pub package: Option<&'static str>,
}

/// Known CLIs (§9.10 table).
pub const KNOWN_CLIS: &[KnownCli] = &[
    KnownCli {
        program: "claude-agent-acp",
        cli: "claude",
        package: Some("@agentclientprotocol/claude-agent-acp"),
    },
    KnownCli {
        program: "codex-acp",
        cli: "codex",
        package: Some("@agentclientprotocol/codex-acp"),
    },
    KnownCli {
        program: "agy",
        cli: "agy",
        package: None,
    },
    KnownCli {
        program: "command-code",
        cli: "command-code",
        package: None,
    },
];

/// Env override of the npm command (gates use a fake npm).
pub const ENV_NPM: &str = "HARNESS_NPM";
/// Shown when npm (Node.js) is missing.
pub const NPM_STEPS: &str =
    "npm was not found and Node.js could not be downloaded. Check the connection to nodejs.org, or install \
Node.js LTS from https://nodejs.org, then install the adapters again.";

const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
const PREFIX_TIMEOUT: Duration = Duration::from_secs(30);
const LINE_MAX: usize = 300;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The known CLI whose launch program is `program` (bare name or path; compared by file stem).
#[must_use]
pub fn known_cli(program: &str) -> Option<&'static KnownCli> {
    let stem = Path::new(program)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())?;
    KNOWN_CLIS.iter().find(|k| k.program == stem)
}

fn search_dirs_cell() -> &'static Mutex<Vec<PathBuf>> {
    static DIRS: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
    DIRS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Directories searched after PATH (npm global bin directories found after an install).
#[must_use]
pub fn extra_search_dirs() -> Vec<PathBuf> {
    search_dirs_cell()
        .lock()
        .map(|d| d.clone())
        .unwrap_or_default()
}

pub(crate) fn add_search_dir(dir: PathBuf) {
    if let Ok(mut dirs) = search_dirs_cell().lock() {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
}

/// Probe of an agent without a resolvable launch program: `AdapterMissing` when the CLI of a known adapter is
/// installed, else `NotInstalled` (never runs a program).
pub(crate) fn probe_missing(spec: &AgentSpec) -> Availability {
    let Some(known) = known_cli(&spec.program) else {
        return Availability::NotInstalled {
            hint: spec.install_hint.clone(),
        };
    };
    let Some(package) = known.package else {
        return Availability::NotInstalled {
            hint: spec.install_hint.clone(),
        };
    };
    match resolve_program(known.cli) {
        Some(cli_path) => Availability::AdapterMissing {
            cli_path: cli_path.to_string_lossy().into_owned(),
            package: package.to_owned(),
            hint: format!(
                "the `{}` CLI is installed but its ACP adapter is not: install {package} (Settings › Agents › Install adapter)",
                known.cli
            ),
        },
        None => Availability::NotInstalled {
            hint: format!(
                "install the `{}` CLI first; Harness then installs its ACP adapter ({package})",
                known.cli
            ),
        },
    }
}

/// Whether Harness can download the cli CLI from its official channel.
#[must_use]
pub fn cli_downloadable(cli: &str) -> bool {
    crate::tools::downloadable(cli)
}

/// The npm command: `HARNESS_NPM` (missing file = no npm), else `npm` on PATH (`npm.cmd` on Windows).
#[must_use]
pub fn npm_command() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os(ENV_NPM).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        return path.is_file().then_some(path);
    }
    resolve_program("npm").or_else(crate::tools::harness_npm)
}

fn installing() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Releases the single-flight slot of a package when the install ends (also on early return).
pub(crate) struct InstallSlot(String);

impl InstallSlot {
    pub(crate) fn take(package: &str) -> Option<Self> {
        let mut set = installing().lock().ok()?;
        set.insert(package.to_owned())
            .then(|| Self(package.to_owned()))
    }
}

impl Drop for InstallSlot {
    fn drop(&mut self) {
        if let Ok(mut set) = installing().lock() {
            set.remove(&self.0);
        }
    }
}

pub(crate) fn npm(program: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(prefix) = crate::tools::npm_prefix() {
        command.env("npm_config_prefix", prefix);
    }
    if let Some(path) = crate::tools::path_env() {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn clip(line: &str) -> String {
    let line = line.trim_end();
    match line.char_indices().nth(LINE_MAX) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

/// `npm prefix -g` → the global bin directory (`<prefix>` on Windows, `<prefix>/bin` elsewhere).
pub(crate) async fn global_bin_dir(program: &Path) -> Option<PathBuf> {
    let output = tokio::time::timeout(PREFIX_TIMEOUT, npm(program, &["prefix", "-g"]).output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let prefix = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())?
        .to_owned();
    let prefix = PathBuf::from(prefix);
    Some(if cfg!(windows) {
        prefix
    } else {
        prefix.join("bin")
    })
}
