use std::sync::{Arc, RwLock};

use harness_remote::SettingsView;
use harness_settings::Settings;
use serde::Serialize;
use serde_json::{json, Value};

use super::settings_rows::{choice, number, toggle as edit};

static MIRROR: RwLock<Option<Arc<Settings>>> = RwLock::new(None);

pub fn mirror(settings: Option<Arc<Settings>>) {
    if let Ok(mut slot) = MIRROR.write() {
        *slot = settings;
    }
}

fn current() -> Option<Arc<Settings>> {
    MIRROR.read().ok().and_then(|slot| slot.clone())
}

pub struct DesktopSettings;

fn label<T: Serialize>(value: &T) -> String {
    let raw = serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut out = String::new();
    for (i, word) in raw.split(['_', '-']).filter(|w| !w.is_empty()).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            if i == 0 {
                out.extend(first.to_uppercase());
            } else {
                out.push(first);
            }
            out.push_str(chars.as_str());
        }
    }
    out
}

fn agent_name(id: &str) -> String {
    match id {
        "claude-code" => "Claude Code".into(),
        "codex" => "Codex".into(),
        "agy" => "Antigravity".into(),
        "api-deepseek" => "DeepSeek".into(),
        other => other.to_owned(),
    }
}

fn on_off(on: bool) -> &'static str {
    if on {
        "On"
    } else {
        "Off"
    }
}

fn row(title: &str, value: impl Into<String>) -> Value {
    json!({ "title": title, "value": value.into() })
}

#[allow(clippy::needless_pass_by_value)]
fn section(title: &str, rows: Vec<Value>) -> Value {
    json!({ "title": title, "rows": rows })
}

fn host() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
}

fn device_count() -> usize {
    super::snapshot().devices.len()
}

fn enabled_agents(s: &Settings) -> usize {
    s.agents.values().filter(|a| a.enabled).count()
}

impl SettingsView for DesktopSettings {
    fn set(&self, path: &str, value: &Value) -> Result<(), String> {
        let s = current().ok_or_else(|| "settings are still loading".to_owned())?;
        if !super::settings_rows::editable(&s, path) {
            return Err(format!("{path} can only be changed on the desktop"));
        }
        super::settings_rows::check_range(path, value)?;
        crate::settings::patch_remote(&s, path, value.clone())
    }

    fn limits(&self) -> Value {
        super::settings_rows::limits()
    }

    fn overview(&self) -> Value {
        let Some(s) = current() else {
            return json!({ "host": host(), "items": {} });
        };
        json!({
            "host": host(),
            "items": {
                "general": label(&s.general.language),
                "appearance": label(&s.appearance.theme),
                "notifications": on_off(s.notifications.task_done),
                "shortcuts": s.shortcuts.len().to_string(),
                "agents": format!("{} enabled", enabled_agents(&s)),
                "orchestra": agent_name(&s.orchestra.default_agent),
                "memory": on_off(s.memory.workspace_memory_enabled),
                "remote": format!("{} devices", device_count()),
            }
        })
    }

    #[allow(clippy::too_many_lines)]
    fn page(&self, page: &str) -> Value {
        let Some(s) = current() else {
            return json!({ "title": page, "subtitle": "", "sections": [] });
        };
        let (title, subtitle, sections) = match page {
            "general" => (
                "General",
                "Language, composer and conversation display",
                vec![
                    section(
                        "Basics",
                        vec![
                            choice("Language", "general.language", &json!(s.general.language), &[(json!("system"), "System"), (json!("en-US"), "English"), (json!("zh-CN"), "中文")]),
                            choice("Send with", "general.composerSend", &json!(s.general.composer_send), &[(json!("enter"), "Enter"), (json!("modEnter"), "Ctrl + Enter")]),
                            choice("Follow-up", "general.followUp", &json!(s.general.follow_up), &[(json!("queue"), "Queue"), (json!("steer"), "Steer")]),
                            edit("Launch at startup", "general.launchAtStartup", s.general.launch_at_startup),
                        ],
                    ),
                    section(
                        "Conversation",
                        vec![
                            edit("Show reasoning", "general.showReasoning", s.general.show_reasoning),
                            edit("Show todos", "general.showTodos", s.general.show_todos),
                            edit("Group exploration", "general.groupExploration", s.general.group_exploration),
                            edit("Group terminal", "general.groupTerminal", s.general.group_terminal),
                            edit("Group file changes", "general.groupFileChanges", s.general.group_file_changes),
                            edit("Auto compact", "general.autoCompact", s.general.auto_compact),
                            edit("Auto archive", "general.autoArchive", s.general.auto_archive),
                        ],
                    ),
                ],
            ),
            "appearance" => (
                "Appearance",
                "Theme, fonts and density",
                vec![
                    section(
                        "Theme",
                        vec![
                            choice("Theme", "appearance.theme", &json!(s.appearance.theme), &[(json!("system"), "System"), (json!("light"), "Light"), (json!("dark"), "Dark")]),
                            row("Accent", s.appearance.accent_color.clone()),
                            choice("Density", "appearance.density", &json!(s.appearance.density), &[(json!("compact"), "Compact"), (json!("comfortable"), "Comfortable")]),
                        ],
                    ),
                    section(
                        "Text",
                        vec![
                            row("Interface font", s.appearance.ui_font.clone()),
                            number("Font size", "appearance.uiFontSize", i64::from(s.appearance.ui_font_size), "px"),
                            number("Zoom", "appearance.zoomPercent", i64::from(s.appearance.zoom_percent), "%"),
                            number("Code font size", "appearance.codeFontSize", i64::from(s.appearance.code_font_size), "px"),
                            edit("Line numbers", "appearance.lineNumbers", s.appearance.line_numbers),
                            edit("Wrap lines", "appearance.wrapLines", s.appearance.wrap_lines),
                            edit("Reduce motion", "appearance.reduceMotion", s.appearance.reduce_motion),
                            edit("Show token usage", "appearance.showTokenUsage", s.appearance.show_token_usage),
                        ],
                    ),
                ],
            ),
            "notifications" | "inbox" => (
                "Notifications",
                "When Harness tells you something finished",
                vec![section(
                    "Alerts",
                    vec![
                        edit("Task done", "notifications.taskDone", s.notifications.task_done),
                        edit("Build done", "notifications.buildDone", s.notifications.build_done),
                        edit("PR review", "notifications.prReview", s.notifications.pr_review),
                        edit("Sound", "notifications.sound", s.notifications.sound),
                        edit("Keep awake", "notifications.keepAwake", s.notifications.keep_awake),
                        edit("Close to tray", "notifications.closeToTray", s.notifications.close_to_tray),
                    ],
                )],
            ),
            "shortcuts" => (
                "Shortcuts",
                "Keyboard bindings on the desktop",
                vec![section(
                    "Bindings",
                    s.shortcuts
                        .iter()
                        .map(|(id, keys)| {
                            json!({ "title": label(&id.as_str()), "value": keys.join(" / "), "kind": "code" })
                        })
                        .collect(),
                )],
            ),
            "agents" => (
                "Agents",
                "Coding agents Harness can run",
                vec![section(
                    "Installed agents",
                    s.agents
                        .iter()
                        .map(|(id, prefs)| {
                            let mut row = edit(
                                &agent_name(id),
                                &format!("agents.{id}.enabled"),
                                prefs.enabled,
                            );
                            row["detail"] = json!(prefs.default_model.clone().unwrap_or_default());
                            row
                        })
                        .collect(),
                )],
            ),
            "orchestra" => (
                "Orchestra",
                "Task board and parallel agents",
                vec![section(
                    "Defaults",
                    vec![
                        row("Default agent", agent_name(&s.orchestra.default_agent)),
                        choice("Default mode", "orchestra.defaultMode", &json!(s.orchestra.default_mode), &[(json!("ask"), "Ask"), (json!("acceptEdits"), "Accept edits"), (json!("machine"), "Machine")]),
                        number("Max concurrent", "orchestra.maxConcurrent", i64::from(s.orchestra.max_concurrent), ""),
                        row("Max per subagent", s.orchestra.max_nested_per_subagent.to_string()),
                        edit("Auto claim", "orchestra.autoClaim", s.orchestra.auto_claim),
                        edit("Snap to grid", "orchestra.snapToGrid", s.orchestra.snap_to_grid),
                    ],
                )],
            ),
            "memory" => (
                "Memory",
                "What agents remember between sessions",
                vec![section(
                    "Memory",
                    vec![edit("Workspace memory", "memory.workspaceMemoryEnabled", s.memory.workspace_memory_enabled)],
                )],
            ),
            "remote" => (
                "Remote",
                "Phone access to this desktop",
                vec![section(
                    "Server",
                    vec![
                        json!({ "title": "Server", "badge": "Running", "tone": "success" }),
                        row("Port", s.remote.port.to_string()),
                        row("Paired devices", device_count().to_string()),
                    ],
                )],
            ),
            "skill" => {
                let skills = crate::skills::summary();
                let rows: Vec<Value> = if skills.is_empty() {
                    vec![row("Installed skills", "None")]
                } else {
                    skills
                        .into_iter()
                        .map(|(name, agents)| {
                            json!({ "title": name, "detail": agents })
                        })
                        .collect()
                };
                (
                    "Skills",
                    "Installed agent skills (read-only)",
                    vec![section("User scope", rows)],
                )
            }
            _ => (page, "", Vec::new()),
        };
        json!({ "title": title, "subtitle": subtitle, "sections": sections })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    out.push(k.to_lowercase());
                    keys(v, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|v| keys(v, out)),
            _ => {}
        }
    }

    #[test]
    fn pages_carry_no_secret_fields() {
        mirror(Some(Arc::new(Settings::default())));
        for page in [
            "general",
            "appearance",
            "notifications",
            "shortcuts",
            "agents",
            "orchestra",
            "memory",
            "remote",
            "mcp",
            "skill",
        ] {
            let mut found = Vec::new();
            keys(&DesktopSettings.page(page), &mut found);
            for k in found {
                assert!(
                    !["key", "token", "secret", "subdomain", "proxy"]
                        .iter()
                        .any(|bad| k.contains(bad)),
                    "{page}: {k}"
                );
            }
        }
        assert_eq!(label(&"ui_font"), "Ui font");
    }
}
