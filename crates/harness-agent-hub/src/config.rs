//! Hub configuration.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use harness_agent_config::AgentKind;
use harness_config::Limits;
use harness_core_api::{AgentDefaults, AgentId};

use crate::agents::{builtin_agents, AgentSpec};
use crate::error::HubError;
use crate::helpers::HelperLaunch;

mod agents_file;
mod policy;

pub use policy::{
    AutoUpdatePolicy, DelegationPolicy, FactCheckPolicy, SharedAuthProbe, SharedFileSearch,
};

/// File name of the agent configuration under the hub home.
pub const AGENTS_FILE: &str = "agents.json";

/// Default idle period after which an unused agent process is stopped (spec §10).
pub const DEFAULT_AGENT_IDLE: Duration = Limits::DEFAULT.agent_idle();
/// Grace period between closing an agent's stdin and killing its process tree.
pub const SHUTDOWN_GRACE: Duration = Limits::DEFAULT.shutdown_grace();

/// Everything the hub needs to start.
#[derive(Debug, Clone)]
pub struct HubConfig {
    /// Data directory (`~/.harness`, or `HARNESS_HOME`).
    pub home: PathBuf,
    /// Configured agents, built-ins first.
    pub agents: Vec<AgentSpec>,
    /// Failover order used until the user sets one.
    pub default_fallback_order: Vec<AgentId>,
    /// Agents that stand for the same service with different API keys (`providers.rs`). A new session asked
    /// for a member takes the members in turn (round robin). Filled when `providers.json` is applied.
    pub key_pools: Vec<Vec<AgentId>>,
    /// Round robin over `key_pools` (off: every session keeps the agent it asked for; failover still walks the
    /// keys). `from_env`: on unless `HARNESS_KEY_ROTATION=off`; `providers.json` `"rotation": "off"` also
    /// turns it off.
    pub rotate_keys: bool,
    /// Idle period before an agent process is stopped.
    pub agent_idle_timeout: Duration,
    /// How helper processes (memory / delegate MCP servers, API provider agent) are launched (§9.9).
    pub helpers: HelperLaunch,
    /// Broadcast capacity of the event stream.
    pub event_capacity: usize,
    /// Apply `<home>/agents.json` in [`crate::Hub::start`] (spec §15.1 request 1). `from_env`: true.
    pub load_agents_file: bool,
    /// Take git tree snapshots around turns for `turn_diff` (spec §15.2). `from_env`: true unless
    /// `HARNESS_TURN_SNAPSHOTS=0`.
    pub turn_snapshots: bool,
    /// Per-agent default mode/model/config for fresh native sessions without an own choice (phase-9-parity.md
    /// §9.1.5, AGT-007). The UI fills it from settings `agents.<id>`; `from_env`: empty.
    pub agent_defaults: HashMap<AgentId, AgentDefaults>,
    /// CLI kind of an agent id for login presence (phase-9-parity.md §9.1.5a); ids not listed map by name
    /// (`claude-code`, `codex`, `agy`). `from_env`: empty.
    pub agent_kinds: HashMap<AgentId, AgentKind>,
    /// Login presence probe (never reads secrets). `from_env`: file/env existence.
    pub auth_probe: SharedAuthProbe,
    /// File-name search borrowed from the UI's workspace index for `@` mentions and the command center
    /// (phase-9-parity.md §9.2.1). `from_env`: none.
    pub file_search: Option<SharedFileSearch>,
    /// Where the CLIs keep slash command files (`list_commands`). `from_env`: `ConfigPaths::from_env`.
    pub config_paths: Option<harness_agent_config::ConfigPaths>,
    /// Read-only history sources for `import_scan` (decision 14A). `from_env`: `~/.zcode` and `~/.claude`.
    pub import_roots: harness_import::ImportRoots,
    /// Smallest `Interval` of a scheduled prompt in seconds. `from_env`: 300.
    pub schedule_min_interval_secs: u32,
    /// Credential store for Antigravity accounts. `from_env`: unavailable (the app injects the OS store).
    pub agy_creds: crate::agy_cred::SharedAgyCredStore,
    /// Subagent limit and nesting (settings `orchestra.maxConcurrent`, `orchestra.nestedSubagents`). The UI keeps
    /// it in step with settings while the hub runs. `from_env`: 4 at once, no nesting.
    pub delegation: DelegationPolicy,
    /// Fact check for deep research reports (docs/specs/harness/deep-research.md, section "Fact check"). The UI
    /// keeps it in step with settings `research.factCheck`. `from_env`: on.
    pub fact_check: FactCheckPolicy,
    /// Automatic update of the npm-installed ACP adapters and agent CLIs (phase-9-parity.md §9.10). The UI
    /// keeps it in step with settings `setup.autoUpdateAdapters`. `from_env`: on.
    pub auto_update: AutoUpdatePolicy,
    /// Failover policy of turns (spec §6.4). `from_env`: [`crate::selector::StickyThenFallback`].
    pub agent_selector: crate::selector::SharedAgentSelector,
}

impl HubConfig {
    /// Configuration from the environment: `HARNESS_HOME` (else `~/.harness`), `HARNESS_AGENT_IDLE_SECS`,
    /// helpers per [`HelperLaunch::from_env`] (standalone binaries; the app replaces them with itself, §9.9).
    ///
    /// # Errors
    ///
    /// When no home directory or helper path can be resolved.
    pub fn from_env() -> Result<Self, HubError> {
        let home = harness_config::harness_home()
            .ok_or_else(|| HubError::Config("no home directory (set HARNESS_HOME)".into()))?;
        let agent_idle_timeout = std::env::var("HARNESS_AGENT_IDLE_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map_or(DEFAULT_AGENT_IDLE, Duration::from_secs);
        let helpers = HelperLaunch::from_env()?;
        let agents = builtin_agents();
        let home_for_paths = home.clone();
        let default_fallback_order = agents.iter().map(|a| a.id.clone()).collect();
        Ok(Self {
            home,
            agents,
            default_fallback_order,
            key_pools: Vec::new(),
            rotate_keys: !std::env::var("HARNESS_KEY_ROTATION").is_ok_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "off" | "0" | "false"
                )
            }),
            agent_idle_timeout,
            helpers,
            event_capacity: Limits::DEFAULT.event_capacity,
            load_agents_file: true,
            turn_snapshots: std::env::var("HARNESS_TURN_SNAPSHOTS")
                .map_or(true, |v| v.trim() != "0"),
            agent_defaults: HashMap::new(),
            agent_kinds: HashMap::new(),
            auth_probe: SharedAuthProbe::default(),
            file_search: None,
            config_paths: harness_agent_config::ConfigPaths::from_env(&home_for_paths, None).ok(),
            import_roots: harness_import::ImportRoots::from_env(),
            schedule_min_interval_secs: Limits::DEFAULT.schedule_min_interval_secs,
            agy_creds: crate::agy_cred::SharedAgyCredStore::default(),
            delegation: DelegationPolicy::default(),
            fact_check: FactCheckPolicy::default(),
            auto_update: AutoUpdatePolicy::default(),
            agent_selector: crate::selector::SharedAgentSelector::default(),
        })
    }

    /// Path of the optional agent configuration file.
    #[must_use]
    pub fn agents_file(&self) -> PathBuf {
        self.home.join(AGENTS_FILE)
    }

    /// Path of the SQLite store.
    #[must_use]
    pub fn db_path(&self) -> PathBuf {
        self.home.join("harness.sqlite")
    }
}
