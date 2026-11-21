//! Agent config page (MCP, skills, commands, hooks, subagents): concept listing, groups and row models.

use super::entry::{agent_kind, concept_of, scope_of, transport_id};
use super::util::opt;
use super::{
    model, sv, with_state, After, AgentKind, AppWindow, ComponentHandle, Concept, ConceptSupport,
    ConfigTarget, ConfigUi, Entry, EntryBody, PlanOp, Req, ResGroup, ResRow, Scope, SharedString,
    State, ROW_PAGE,
};

impl State {
    // ---- agent config (MCP, skills, commands, hooks, subagents) ------------------------------------------

    pub(super) fn target(&self) -> (Option<Concept>, ConfigTarget) {
        let cfg = self.window.global::<ConfigUi>();
        (
            concept_of(&cfg.get_concept()),
            ConfigTarget::new(agent_kind(&cfg.get_agent()), scope_of(&cfg.get_scope())),
        )
    }

    pub(super) fn reload_config(&mut self) {
        let (Some(concept), target) = self.target() else {
            return;
        };
        self.seq += 1;
        let cfg = self.window.global::<ConfigUi>();
        cfg.set_loading(true);
        cfg.set_error(SharedString::new());
        self.send(Req::List {
            seq: self.seq,
            concept,
            target,
        });
    }

    pub(super) fn config_listed(
        &mut self,
        seq: u64,
        support: Option<ConceptSupport>,
        result: Result<Vec<Entry>, String>,
    ) {
        if seq != self.seq {
            return;
        }
        let cfg = self.window.global::<ConfigUi>();
        cfg.set_loading(false);
        let scope = scope_of(&cfg.get_scope());
        let supported = support.as_ref().is_some_and(|c| c.user || c.workspace);
        let scope_supported = support.as_ref().is_some_and(|c| match scope {
            Scope::User => c.user,
            Scope::Workspace => c.workspace,
        });
        cfg.set_supported(supported);
        cfg.set_scope_supported(scope_supported);
        cfg.set_can_disable(support.as_ref().is_some_and(|c| c.can_disable));
        cfg.set_reason(sv(support
            .as_ref()
            .and_then(|c| c.reason.clone())
            .unwrap_or_default()));
        self.support = support;
        match result {
            Ok(entries) => self.entries = entries,
            Err(error) => {
                self.entries.clear();
                if supported && scope_supported {
                    cfg.set_error(sv(&error));
                    self.status(&error, true);
                }
            }
        }
        self.refresh_rows();
    }

    pub(super) fn refresh_rows(&self) {
        let cfg = self.window.global::<ConfigUi>();
        let query = cfg.get_query().to_lowercase();
        let builtins: &[(&str, &str)] =
            if cfg.get_concept() == "mcp" && cfg.get_agent() == "harness" {
                &BUILTIN_MCP
            } else {
                &[]
            };
        let builtin_rows = builtins
            .iter()
            .filter(|(name, summary)| {
                query.is_empty() || name.contains(&query) || summary.to_lowercase().contains(&query)
            })
            .map(|(name, summary)| ResRow {
                name: sv(*name),
                scope: sv("user"),
                enabled: true,
                toggleable: false,
                summary: sv(*summary),
                path: SharedString::new(),
                group: sv("Added by Harness to every session"),
                builtin: true,
                detail: sv("stdio"),
                extra: SharedString::new(),
            });
        let matches = |e: &&Entry| {
            query.is_empty()
                || e.name.to_lowercase().contains(&query)
                || e.summary.to_lowercase().contains(&query)
        };
        let total = self.entries.iter().filter(matches).count();
        let limit = if cfg.get_concept() == "hook" {
            usize::MAX
        } else {
            usize::try_from(cfg.get_limit().max(ROW_PAGE)).unwrap_or(usize::MAX)
        };
        cfg.set_total(i32::try_from(self.entries.len()).unwrap_or(i32::MAX));
        cfg.set_hidden(i32::try_from(total.saturating_sub(limit)).unwrap_or(i32::MAX));
        let rows: Vec<ResRow> = builtin_rows
            .chain(
                self.entries
                    .iter()
                    .filter(matches)
                    .take(limit)
                    .map(|e| ResRow {
                        name: sv(&e.name),
                        scope: sv(match e.scope {
                            Scope::User => "user",
                            Scope::Workspace => "workspace",
                        }),
                        enabled: e.enabled.unwrap_or(true),
                        toggleable: e.enabled.is_some(),
                        summary: sv(&e.summary),
                        path: sv(e.source_path.display().to_string()),
                        group: sv(match &e.body {
                            EntryBody::Hook(h) => h.event.clone(),
                            _ if builtins.is_empty() => String::new(),
                            _ => "Configured".to_owned(),
                        }),
                        builtin: false,
                        detail: sv(match &e.body {
                            EntryBody::Mcp(m) => transport_id(m.transport).to_owned(),
                            EntryBody::Subagent(a) => opt(a.model.as_ref()),
                            EntryBody::Command(c) => opt(c.argument_hint.as_ref()),
                            EntryBody::Hook(h) => opt(h.matcher.as_ref()),
                            _ => String::new(),
                        }),
                        extra: sv(match &e.body {
                            EntryBody::Subagent(a) => opt(a.tools.as_ref()),
                            EntryBody::Hook(h) => h.command.clone(),
                            _ => String::new(),
                        }),
                    }),
            )
            .collect();
        // Hooks page: one card per event, in first-seen order.
        let mut groups: Vec<ResGroup> = Vec::new();
        for r in &rows {
            match groups.iter_mut().find(|g| g.name == r.group) {
                Some(g) => g.count += 1,
                None => groups.push(ResGroup {
                    name: r.group.clone(),
                    count: 1,
                }),
            }
        }
        cfg.set_groups(model(groups));
        cfg.set_rows(model(rows));
    }
}

const BUILTIN_MCP: [(&str, &str); 3] = [
    (
        "harness-memory",
        "Global and project memory (MEMORY.md) and the working notes of each session",
    ),
    (
        "harness-delegate",
        "Subagent delegation and coordination between sessions",
    ),
    ("harness-computer", "Screen, mouse and keyboard control"),
];

#[allow(clippy::too_many_lines)]
pub(super) fn wire(window: &AppWindow) {
    let cfg = window.global::<ConfigUi>();
    cfg.on_select_agent(|agent| {
        with_state(|s| {
            let cfg = s.window.global::<ConfigUi>();
            cfg.set_agent(agent.clone());
            cfg.set_limit(ROW_PAGE);
            if agent == "harness" {
                cfg.set_scope(sv("user"));
            }
            s.close_editor();
            s.reload_config();
        });
    });
    cfg.on_select_scope(|scope| {
        with_state(|s| {
            s.window.global::<ConfigUi>().set_scope(scope);
            s.window.global::<ConfigUi>().set_limit(ROW_PAGE);
            s.close_editor();
            s.reload_config();
        });
    });
    cfg.on_search(|_| {
        with_state(|s| {
            s.window.global::<ConfigUi>().set_limit(ROW_PAGE);
            s.refresh_rows();
        });
    });
    cfg.on_show_more(|| {
        with_state(|s| {
            let cfg = s.window.global::<ConfigUi>();
            cfg.set_limit(cfg.get_limit().saturating_add(ROW_PAGE));
            s.refresh_rows();
        });
    });
    cfg.on_refresh(|| with_state(State::reload_config));
    cfg.on_edit(|name| with_state(|s| s.open_editor(Some(&name))));
    cfg.on_create(|| with_state(|s| s.open_editor(None)));
    cfg.on_toggle(|name, on| {
        with_state(|s| {
            let (Some(concept), target) = s.target() else {
                return;
            };
            s.after = After::Config;
            s.send(Req::Plan {
                title: "toggle",
                op: PlanOp::Toggle {
                    concept,
                    target,
                    name: name.to_string(),
                    on,
                },
            });
        });
    });
    cfg.on_remove(|name| {
        with_state(|s| {
            let (Some(concept), target) = s.target() else {
                return;
            };
            s.after = After::Config;
            s.send(Req::Plan {
                title: "delete",
                op: PlanOp::Delete {
                    concept,
                    target,
                    name: name.to_string(),
                },
            });
        });
    });
    cfg.on_copy_to_harness(|name| {
        with_state(|s| {
            let (Some(concept), from) = s.target() else {
                return;
            };
            s.after = After::Config;
            s.send(Req::Plan {
                title: "copy",
                op: PlanOp::Copy {
                    concept,
                    from,
                    to: ConfigTarget {
                        agent: AgentKind::Harness,
                        scope: Scope::User,
                    },
                    name: name.to_string(),
                },
            });
        });
    });
    cfg.on_import(|source| {
        with_state(|s| {
            let sources = match source.as_str() {
                "claude-code" => vec![AgentKind::ClaudeCode],
                "codex" => vec![AgentKind::Codex],
                "agy" => vec![AgentKind::Agy],
                _ => vec![AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Agy],
            };
            s.after = After::Config;
            s.send(Req::Plan {
                title: "import",
                op: PlanOp::ImportMcp(sources),
            });
        });
    });
}
