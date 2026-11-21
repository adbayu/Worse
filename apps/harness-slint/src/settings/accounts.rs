//! Accounts and API providers on the Agents page: login profiles and key pools (never the key itself).

use super::util::command_code_keys;
use super::{
    agy, api, model, with_state, AccountRow, AgentsUi, AppWindow, ComponentHandle, ProviderRow,
    SharedString,
};

/// Account list (or the error of the last account call) from the hub (phase-9-parity.md §9.6).
pub fn accounts(result: Result<Vec<api::AccountInfo>, String>) {
    with_state(|s| {
        let ui = s.window.global::<AgentsUi>();
        match result {
            Ok(rows) => {
                ui.set_account_error(SharedString::new());
                s.accounts = rows;
            }
            Err(error) => ui.set_account_error(error.into()),
        }
        let rows: Vec<AccountRow> = s
            .accounts
            .iter()
            .map(|a| AccountRow {
                id: a.id.0.clone().into(),
                label: a.label.clone().into(),
                auth: match a.auth {
                    api::AuthPresence::Present => 0,
                    api::AuthPresence::Absent => 1,
                    api::AuthPresence::Unknown => 2,
                },
                loaded: a.loaded,
            })
            .collect();
        ui.set_accounts(model(rows));
    });
}

/// Provider list (or the error of the last provider call) from the hub (phase-9-parity.md §9.7).
pub fn providers(result: Result<Vec<api::ProviderInfo>, String>) {
    with_state(|s| {
        let ui = s.window.global::<AgentsUi>();
        match result {
            Ok(rows) => {
                ui.set_provider_error(SharedString::new());
                s.providers = rows;
            }
            Err(error) => ui.set_provider_error(error.into()),
        }
        let rows: Vec<ProviderRow> = s
            .providers
            .iter()
            .map(|p| ProviderRow {
                id: p.id.0.clone().into(),
                label: p.label.clone().into(),
                model: p.model.clone().into(),
                endpoint: p.base_url.clone().into(),
                has_key: p.has_key,
                loaded: p.loaded,
                kind: p.kind.clone().into(),
            })
            .collect();
        ui.set_providers(model(rows));
    });
}

/// Opens a console that runs `claude` with the account's `CLAUDE_CONFIG_DIR`, where the user signs in
/// (`/login`). Harness never sees the credentials (§9.6).
pub(super) fn open_account_login(dir: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // The outer cmd has no window; `start` opens a new console for the login.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let child = std::process::Command::new("cmd")
            .args(["/C", "start", "Claude login", "cmd", "/K", "claude"])
            .env("CLAUDE_CONFIG_DIR", dir)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        reap(child);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let quoted = format!("'{}'", dir.replace('\'', r"'\''"));
        let script = format!("CLAUDE_CONFIG_DIR={quoted} claude");
        let child = if cfg!(target_os = "macos") {
            let apple = script.replace('\\', "\\\\").replace('"', "\\\"");
            std::process::Command::new("osascript")
                .args([
                    "-e",
                    &format!("tell application \"Terminal\" to do script \"{apple}\""),
                    "-e",
                    "tell application \"Terminal\" to activate",
                ])
                .spawn()?
        } else {
            std::process::Command::new("x-terminal-emulator")
                .args(["-e", "sh", "-c", &format!("{script}; exec sh")])
                .spawn()?
        };
        reap(child);
        Ok(())
    }
}

/// Waits for the launcher on a thread so it is reaped (it exits as soon as the console is open).
pub(super) fn reap(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

#[allow(clippy::too_many_lines)]
pub(super) fn wire(window: &AppWindow) {
    let agents = window.global::<AgentsUi>();
    agents.on_add_account(|label| {
        let label = label.trim().to_owned();
        if label.is_empty() {
            return;
        }
        with_state(|s| {
            s.window
                .global::<AgentsUi>()
                .set_account_label(SharedString::new());
        });
        crate::bridge::send(crate::core::Command::Accounts(
            crate::core::AccountsCall::Add(label),
        ));
    });
    agents.on_remove_account(|id| {
        crate::bridge::send(crate::core::Command::Accounts(
            crate::core::AccountsCall::Remove(id.to_string()),
        ));
    });
    agents.on_login_account(|id| {
        with_state(|s| {
            let ui = s.window.global::<AgentsUi>();
            let Some(dir) = s
                .accounts
                .iter()
                .find(|a| a.id.as_str() == id.as_str())
                .map(|a| a.profile_dir.clone())
            else {
                return;
            };
            match open_account_login(&dir) {
                Ok(()) => ui.set_account_error(SharedString::new()),
                Err(error) => ui.set_account_error(format!("open console: {error}").into()),
            }
        });
    });
    agents.on_add_provider(|| {
        let mut requests = Vec::new();
        with_state(|s| {
            let ui = s.window.global::<AgentsUi>();
            let label = ui.get_provider_label().trim().to_owned();
            let kind = ui.get_provider_kind().trim().to_owned();
            if kind == "command-code" {
                // One entry per key (one per line): each key becomes its own Command Code agent, and
                // sessions take them in turn.
                let keys = command_code_keys(&ui.get_provider_key());
                if label.is_empty() || keys.is_empty() {
                    return;
                }
                let many = keys.len() > 1;
                requests = keys
                    .into_iter()
                    .enumerate()
                    .map(|(i, api_key)| api::AddProvider {
                        label: if many {
                            format!("{label} {}", i + 1)
                        } else {
                            label.clone()
                        },
                        base_url: String::new(),
                        model: String::new(),
                        api_key,
                        kind: kind.clone(),
                    })
                    .collect();
            } else {
                // One entry per key (one per line). Entries with the same endpoint and model form a pool
                // that new sessions take in turn (round robin), like Command Code keys. No key at all is
                // fine for a local server.
                let base_url = ui.get_provider_url().trim().to_owned();
                let model = ui.get_provider_model().trim().to_owned();
                if label.is_empty() || base_url.is_empty() || model.is_empty() {
                    return;
                }
                let mut keys = command_code_keys(&ui.get_provider_key());
                if keys.is_empty() {
                    keys.push(String::new());
                }
                let many = keys.len() > 1;
                requests = keys
                    .into_iter()
                    .enumerate()
                    .map(|(i, api_key)| api::AddProvider {
                        label: if many {
                            format!("{label} {}", i + 1)
                        } else {
                            label.clone()
                        },
                        base_url: base_url.clone(),
                        model: model.clone(),
                        api_key,
                        kind: String::new(),
                    })
                    .collect();
            }
            // 提交后立即清空输入框，密钥不在界面里停留。
            for clear in [
                AgentsUi::set_provider_label,
                AgentsUi::set_provider_url,
                AgentsUi::set_provider_model,
                AgentsUi::set_provider_key,
                AgentsUi::set_provider_kind,
            ] {
                clear(&ui, SharedString::new());
            }
            ui.set_provider_preset(SharedString::new());
            ui.set_provider_edit_url(false);
            ui.set_provider_open(false);
        });
        for request in requests {
            crate::bridge::send(crate::core::Command::Providers(
                crate::core::ProvidersCall::Add(request),
            ));
        }
    });
    agents.on_remove_provider(|id| {
        crate::bridge::send(crate::core::Command::Providers(
            crate::core::ProvidersCall::Remove(id.to_string()),
        ));
    });
    agents.on_refresh_accounts(|| {
        crate::bridge::send(crate::core::Command::Providers(
            crate::core::ProvidersCall::List,
        ));
        crate::bridge::send(crate::core::Command::Accounts(
            crate::core::AccountsCall::List,
        ));
        crate::bridge::send(crate::core::Command::AgentsOverview);
        agy::refresh();
    });
}
