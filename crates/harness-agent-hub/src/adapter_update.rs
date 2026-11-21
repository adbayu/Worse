//! Automatic update of the npm-installed ACP adapters and agent CLIs (phase-9-parity.md §9.10).
//!
//! Owner: this module owns the periodic update check, the "last check" stamp and the version comparison. It
//! only ever upgrades a package that is already installed globally, and only while the user has switched
//! auto-update on (`setup.autoUpdateAdapters`); the first install stays a click in the setup dialog.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Weak;
use std::time::Duration;

use harness_core_api::{AdapterInstallStatus, AgentId};

use crate::hub::Hub;
use crate::setup::{global_bin_dir, known_cli, npm, npm_command, InstallSlot, KNOWN_CLIS};

const STATE_FILE: &str = "adapter-update.json";
const FIRST_DELAY: Duration = Duration::from_secs(60);
const TICK: Duration = Duration::from_secs(600);
const CHECK_EVERY_SECS: i64 = 24 * 60 * 60;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// npm packages of the agent CLIs themselves, with the launch program of the agent they belong to. A CLI from
/// a native installer is absent from `npm ls -g` and updates itself, so it is never touched here.
const CLI_PACKAGES: &[(&str, &str)] = &[
    ("@anthropic-ai/claude-code", "claude-agent-acp"),
    ("@openai/codex", "codex-acp"),
];

/// A version as its numeric parts (`1.2.3-beta.1` → `[1, 2, 3]`).
fn parse_version(text: &str) -> Option<Vec<u64>> {
    let text = text.trim().trim_start_matches('v');
    let core = text.split(['-', '+']).next()?;
    let mut parts = Vec::new();
    for part in core.split('.') {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        parts.push(digits.parse().ok()?);
    }
    (!parts.is_empty()).then_some(parts)
}

/// Whether `latest` is a later version than `installed` (missing parts count as 0).
fn is_newer(latest: &[u64], installed: &[u64]) -> bool {
    for index in 0..latest.len().max(installed.len()) {
        let new = latest.get(index).copied().unwrap_or(0);
        let old = installed.get(index).copied().unwrap_or(0);
        if new != old {
            return new > old;
        }
    }
    false
}

/// Whether a check is due: never checked, or the last check is at least `interval` seconds ago.
fn is_due(last_check: i64, now: i64, interval: i64) -> bool {
    last_check <= 0 || now.saturating_sub(last_check) >= interval
}

/// Every globally installed package with its version, from one `npm ls -g --depth=0 --json`.
fn parse_globals(json: &str) -> Option<BTreeMap<String, Vec<u64>>> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.as_object()?;
    let Some(dependencies) = value
        .get("dependencies")
        .and_then(serde_json::Value::as_object)
    else {
        return Some(BTreeMap::new());
    };
    let installed = dependencies
        .iter()
        .filter_map(|(name, entry)| {
            let version = parse_version(entry.get("version")?.as_str()?)?;
            Some((name.clone(), version))
        })
        .collect();
    Some(installed)
}

/// One npm package the updater may upgrade, with the agent whose install events report it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    package: &'static str,
    agent: AgentId,
}

/// Drops every package npm does not list: auto-update upgrades what the user has, it never installs.
fn select_targets(targets: Vec<Target>, installed: &BTreeMap<String, Vec<u64>>) -> Vec<Target> {
    targets
        .into_iter()
        .filter(|target| installed.contains_key(target.package))
        .collect()
}

fn agent_for(hub: &Hub, program: &str) -> Option<AgentId> {
    hub.ctx
        .config
        .agents
        .iter()
        .find(|spec| known_cli(&spec.program).is_some_and(|known| known.program == program))
        .map(|spec| spec.id.clone())
}

/// The adapter package of every configured agent.
fn adapter_targets(hub: &Hub) -> Vec<Target> {
    KNOWN_CLIS
        .iter()
        .filter_map(|known| {
            let package = known.package?;
            let agent = agent_for(hub, known.program)?;
            Some(Target { package, agent })
        })
        .collect()
}

/// The CLI package of every configured agent.
fn cli_targets(hub: &Hub) -> Vec<Target> {
    CLI_PACKAGES
        .iter()
        .filter_map(|(package, program)| {
            let agent = agent_for(hub, program)?;
            Some(Target { package, agent })
        })
        .collect()
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct UpdateState {
    last_check: i64,
}

async fn read_state(path: &Path) -> UpdateState {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => UpdateState::default(),
    }
}

async fn write_state(path: &Path, state: &UpdateState) {
    let Ok(bytes) = serde_json::to_vec(state) else {
        return;
    };
    if let Err(error) = tokio::fs::write(path, bytes).await {
        tracing::warn!(%error, "adapter update state not written");
    }
}

async fn npm_stdout(program: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(COMMAND_TIMEOUT, npm(program, args).output())
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn latest_version(program: &Path, package: &str) -> Option<String> {
    let text = npm_stdout(program, &["view", package, "version"]).await?;
    let last = text.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    Some(last.to_owned())
}

/// Checks every installed adapter and CLI of a configured agent and upgrades the outdated ones.
async fn check(hub: &Hub) -> Result<(), String> {
    let Some(npm_path) = npm_command() else {
        return Ok(());
    };
    let Some(listing) = npm_stdout(&npm_path, &["ls", "-g", "--depth=0", "--json"]).await else {
        tracing::warn!("npm ls -g did not answer; the update check is skipped");
        return Ok(());
    };
    let Some(installed) = parse_globals(&listing) else {
        tracing::warn!("npm ls -g returned no listing; the update check is skipped");
        return Ok(());
    };
    let mut targets = adapter_targets(hub);
    targets.extend(cli_targets(hub));
    for target in select_targets(targets, &installed) {
        let package = target.package;
        let Some(current) = installed.get(package) else {
            continue;
        };
        let Some(latest_text) = latest_version(&npm_path, package).await else {
            continue;
        };
        let Some(latest) = parse_version(&latest_text) else {
            tracing::warn!(%package, %latest_text, "npm reported an unparsable version");
            continue;
        };
        if !is_newer(&latest, current) {
            continue;
        }
        upgrade(hub, &target.agent, package, &latest_text, &npm_path).await;
    }
    Ok(())
}

async fn upgrade(hub: &Hub, agent: &AgentId, package: &str, latest: &str, npm_path: &Path) {
    let Some(_slot) = InstallSlot::take(package) else {
        tracing::info!(%package, "adapter update skipped: an install is running");
        return;
    };
    tracing::info!(%package, %latest, "adapter update started");
    let code = hub.run_npm_install(agent, package, npm_path).await;
    match code {
        Ok(Some(0)) => {
            if let Some(dir) = global_bin_dir(npm_path).await {
                crate::setup::add_search_dir(dir);
            }
            tracing::info!(%package, %latest, "adapter update finished");
            hub.install_event(
                agent,
                package,
                AdapterInstallStatus::Succeeded,
                None,
                Some(format!("{package} updated to {latest}")),
            );
        }
        Ok(code) => {
            let message = format!(
                "npm install -g {package} failed (exit code {})",
                code.map_or_else(|| "none".to_owned(), |c| c.to_string())
            );
            tracing::warn!(%package, %message, "adapter update failed");
            hub.install_event(
                agent,
                package,
                AdapterInstallStatus::Failed,
                None,
                Some(message),
            );
        }
        Err(message) => {
            tracing::warn!(%package, %message, "adapter update failed");
            hub.install_event(
                agent,
                package,
                AdapterInstallStatus::Failed,
                None,
                Some(message),
            );
        }
    }
}

/// Keeps the adapters and CLIs current while the hub runs: a first check a minute after start, then at most one
/// check per day, and only while auto-update is on. Errors are logged and retried on the next tick.
pub(crate) async fn track(hub: Weak<Hub>) {
    let Some(started) = hub.upgrade() else {
        return;
    };
    let path = started.ctx.config.home.join(STATE_FILE);
    drop(started);
    tokio::time::sleep(FIRST_DELAY).await;
    loop {
        let Some(hub) = hub.upgrade() else {
            return;
        };
        let state = read_state(&path).await;
        if hub.ctx.config.auto_update.on()
            && is_due(state.last_check, crate::util::now_secs(), CHECK_EVERY_SECS)
        {
            if let Err(error) = check(&hub).await {
                tracing::warn!(%error, "adapter update check failed");
            }
            write_state(
                &path,
                &UpdateState {
                    last_check: crate::util::now_secs(),
                },
            )
            .await;
        }
        drop(hub);
        tokio::time::sleep(TICK).await;
    }
}

#[cfg(test)]
mod tests;
