//! Agents page: cards, connection tests, and the hub overview / option rows behind them.

use super::util::opt;
use super::{
    api, model, sv, with_state, AgentCard, AgentOptions, AgentsUi, AppWindow, ComponentHandle,
    SelectOption, State, Value,
};

static APPLIED: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> =
    std::sync::Mutex::new(None);

/// Custom programs the running hub was started with (agent id → path, "" = PATH).
pub fn programs_applied(map: std::collections::HashMap<String, String>) {
    *APPLIED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(map);
}

fn restart_needed(id: &str, wanted: &str) -> bool {
    APPLIED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|map| map.get(id).map_or("", String::as_str) != wanted)
}

impl State {
    // ---- agents ------------------------------------------------------------------------------------------

    pub(super) fn refresh_cards(&self) {
        let cards: Vec<AgentCard> = self
            .agents
            .iter()
            .map(|(id, name, installed)| {
                let prefs = self.settings.agent(id);
                let empty = AgentOptions::default();
                let opts = self.options.get(id).unwrap_or(&empty);
                let options = |list: &[(String, String)]| {
                    model(
                        list.iter()
                            .map(|(id, label)| SelectOption {
                                id: sv(id),
                                label: sv(label),
                            })
                            .collect(),
                    )
                };
                let label = |list: &[(String, String)], current: &Option<String>| {
                    current.as_ref().map_or_else(String::new, |c| {
                        list.iter()
                            .find(|(id, _)| id == c)
                            .map_or_else(|| c.clone(), |(_, l)| l.clone())
                    })
                };
                let (program, args, env) = self.launch.get(id).cloned().unwrap_or_default();
                let custom = prefs.program.clone().unwrap_or_default().trim().to_owned();
                AgentCard {
                    id: sv(id),
                    name: sv(name),
                    installed: *installed,
                    adapter_missing: !*installed && self.adapter_missing.contains(id),
                    version: sv(self.overview.get(id).map_or("", |o| o.0.as_str())),
                    auth: self
                        .overview
                        .get(id)
                        .map(|o| o.1)
                        .or_else(|| self.auth.get(id).copied())
                        .unwrap_or(2),
                    enabled: prefs.enabled,
                    mode: sv(opt(prefs.default_mode.as_ref())),
                    model: sv(opt(prefs.default_model.as_ref())),
                    effort: sv(opt(prefs.default_effort.as_ref())),
                    modes: options(&opts.modes),
                    models: options(&opts.models),
                    efforts: options(&opts.efforts),
                    mode_label: sv(label(&opts.modes, &prefs.default_mode)),
                    model_label: sv(label(&opts.models, &prefs.default_model)),
                    effort_label: sv(label(&opts.efforts, &prefs.default_effort)),
                    program: sv(if custom.is_empty() {
                        program
                    } else {
                        custom.clone()
                    }),
                    custom_program: !custom.is_empty(),
                    restart: restart_needed(id, &custom),
                    args: sv(args),
                    env: sv(env),
                    test_state: self.tests.get(id).map_or(0, |t| t.0),
                    test_latency: self.tests.get(id).map_or(0, |t| t.1),
                    test_error: sv(self.tests.get(id).map_or("", |t| t.2.as_str())),
                }
            })
            .collect();
        self.window.global::<AgentsUi>().set_cards(model(cards));
        let disabled: Vec<String> = self
            .agents
            .iter()
            .filter(|(id, _, _)| !self.settings.agent(id).enabled)
            .map(|(id, _, _)| id.clone())
            .collect();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            crate::bridge::set_disabled_agents(disabled);
        });
    }

    pub(super) fn agents_loaded(
        &mut self,
        auth: Vec<(String, i32)>,
        launch: Vec<(String, String, String, String)>,
    ) {
        self.auth = auth.into_iter().collect();
        self.launch = launch
            .into_iter()
            .map(|(id, program, args, env)| (id, (program, args, env)))
            .collect();
        self.refresh_cards();
    }
}

/// Agent list from the core (bridge): (id, display name, installed).
pub fn agents_changed(list: Vec<(String, String, bool)>) {
    with_state(|s| {
        s.agents = list;
        s.refresh_cards();
    });
}

/// Hub Agents overview rows (version, login presence).
pub fn agents_overview(rows: Vec<api::AgentOverview>) {
    with_state(|s| {
        s.adapter_missing = rows
            .iter()
            .filter(|r| r.adapter_missing)
            .map(|r| r.id.0.clone())
            .collect();
        s.overview = rows
            .into_iter()
            .map(|r| {
                let auth = match r.auth {
                    api::AuthPresence::Present => 0,
                    api::AuthPresence::Absent => 1,
                    api::AuthPresence::Unknown => 2,
                };
                (r.id.0, (r.version.unwrap_or_default(), auth))
            })
            .collect();
        s.refresh_cards();
    });
}

/// Result of a connection test started from the Agents page.
pub fn agent_tested(result: api::AgentTestResult) {
    with_state(|s| {
        let state = if result.ok { 2 } else { 3 };
        let latency = i32::try_from(result.latency_ms).unwrap_or(i32::MAX);
        s.tests.insert(
            result.agent.0,
            (state, latency, result.error.unwrap_or_default()),
        );
        s.refresh_cards();
    });
}

/// Mode / model / effort lists of one agent (hub `agent_options`).
#[allow(clippy::similar_names)] // modes / models are the protocol's names.
pub fn agent_options(
    agent: &str,
    modes: Vec<(String, String)>,
    models: Vec<(String, String)>,
    efforts: Vec<(String, String)>,
) {
    with_state(|s| {
        s.options.insert(
            agent.to_string(),
            AgentOptions {
                modes,
                models,
                efforts,
            },
        );
        s.refresh_cards();
    });
}

pub(super) fn wire(window: &AppWindow) {
    let agents = window.global::<AgentsUi>();
    agents.on_set_enabled(|id, on| {
        with_state(|s| s.patch(&format!("agents.{id}.enabled"), Value::from(on)));
    });
    agents.on_set_default(|id, field, value| {
        // "" = the menu's "Agent default" row: clear the stored default (null), never store an empty id.
        let value = if value.is_empty() {
            Value::Null
        } else {
            Value::from(value.as_str())
        };
        with_state(|s| s.patch(&format!("agents.{id}.{field}"), value));
    });
    agents.on_refresh(|| with_state(|s| s.open_page("agents")));
    agents.on_pick_program(|id| {
        let id = id.to_string();
        // 文件选择框会阻塞，放到独立线程；选中的路径作为设置写回。
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new();
            if cfg!(windows) {
                dialog = dialog.add_filter("Programs", &["exe", "cmd", "bat"]);
            }
            if let Some(path) = dialog.pick_file() {
                let path = path.to_string_lossy().into_owned();
                let _ = slint::invoke_from_event_loop(move || {
                    with_state(|s| s.patch(&format!("agents.{id}.program"), Value::from(path)));
                });
            }
        });
    });
    agents.on_clear_program(|id| {
        with_state(|s| s.patch(&format!("agents.{id}.program"), Value::Null));
    });
    agents.on_test(|id| {
        with_state(|s| {
            s.tests.insert(id.to_string(), (1, 0, String::new()));
            s.refresh_cards();
        });
        crate::bridge::send(crate::core::Command::TestAgent(id.to_string()));
    });
}
