use super::{Deserialize, Serialize};

/// Notifications and window behaviour.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NotificationSettings {
    pub task_done: bool,
    pub sound: bool,
    pub close_to_tray: bool,
    pub keep_awake: bool,
    /// Notify when cargo build / CI finishes (settings S1).
    pub build_done: bool,
    /// Notify when a pull request asks for a review (settings S1).
    pub pr_review: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            task_done: true,
            sound: true,
            close_to_tray: false,
            keep_awake: false,
            build_done: true,
            pr_review: true,
        }
    }
}

/// Update channel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateSettings {
    pub preview_channel: bool,
    pub auto_download: bool,
}

/// Per-agent preferences (AGT-002, AGT-007). Program/args/env live in `agents.json` (hub-owned launch config).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentPrefs {
    pub enabled: bool,
    pub default_mode: Option<String>,
    pub default_model: Option<String>,
    /// Value for the agent's `effort`-like config option, when it advertises one.
    pub default_effort: Option<String>,
}

impl Default for AgentPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            default_mode: None,
            default_model: None,
            default_effort: None,
        }
    }
}

/// Memory page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MemorySettings {
    pub workspace_memory_enabled: bool,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            workspace_memory_enabled: true,
        }
    }
}

/// First-run setup state (phase-9-parity.md §9.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SetupSettings {
    /// The first-run "Set up agents" dialog was handled (closed or not needed).
    pub first_run_done: bool,
    /// Keep the npm-installed ACP adapters up to date (phase-9-parity.md §9.10). Default on.
    pub auto_update_adapters: bool,
}

impl Default for SetupSettings {
    fn default() -> Self {
        Self {
            first_run_done: false,
            auto_update_adapters: true,
        }
    }
}

/// Phone access (phase-12-remote-phone.md §3): local HTTP/WS server plus the `cftunnel` supervisor.
/// Loopback only; a tunnel is started from Settings, never at launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RemoteSettings {
    /// The local server should run (started from the Settings page, not at launch).
    pub enabled: bool,
    pub port: u16,
    /// Tunnel helper binary: `cftunnel http <port> --name <subdomain>`.
    pub tunnel_bin: String,
    /// Named tunnel subdomain (empty = quick tunnel with a random host).
    pub subdomain: String,
    /// Zone appended when the tunnel prints no URL: `https://<subdomain>.<tunnel_domain>`.
    pub tunnel_domain: String,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 7878,
            tunnel_bin: "cftunnel".into(),
            subdomain: String::new(),
            tunnel_domain: String::new(),
        }
    }
}

/// Orchestra page (settings redesign S3): how Harness delegates work to subagents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OrchestraSettings {
    /// Direct subagents running at once under one top-level chat (at least 1, no upper limit).
    pub max_concurrent: u16,
    /// Subagents may start subagents of their own.
    pub nested_subagents: bool,
    /// Subagents one subagent may run at once under itself, its own quota apart from `max_concurrent` (at least 1).
    pub max_nested_per_subagent: u16,
    /// Agent id used when a task does not name one (empty = first connected agent).
    pub default_agent: String,
    /// Permission mode subagents start in: "ask" | "acceptEdits" | "machine".
    pub default_mode: String,
    /// Subagents claim files before editing.
    pub auto_claim: bool,
    /// Markdown appended to every delegated task.
    pub instructions: String,
    /// Orchestra canvas zoom in percent (40..=200).
    pub canvas_zoom: u16,
    pub snap_to_grid: bool,
}

impl Default for OrchestraSettings {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            nested_subagents: false,
            max_nested_per_subagent: 4,
            default_agent: String::new(),
            default_mode: "acceptEdits".into(),
            auto_claim: true,
            instructions: String::new(),
            canvas_zoom: 100,
            snap_to_grid: true,
        }
    }
}

/// Commit gate for the dependency audit (security-panel.md §4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommitGate {
    #[default]
    Off,
    Warn,
    Block,
}

/// Security page (security-panel.md §5): dependency audit and file checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SecuritySettings {
    pub commit_gate: CommitGate,
    pub scan_on_lockfile_change: bool,
    pub offline_scan: bool,
    pub show_diff_badge: bool,
}

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            commit_gate: CommitGate::Off,
            scan_on_lockfile_change: false,
            offline_scan: false,
            show_diff_badge: true,
        }
    }
}
