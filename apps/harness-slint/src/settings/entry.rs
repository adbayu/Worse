//! Agent config entries: concept and target mapping, form field lists, form <-> entry conversion.

use super::util::{lines_of, non_empty, opt, pairs};
use super::{
    AgentKind, CmdEntry, Concept, EntryBody, Hook, McpServer, McpTransport, Scope, Skill, Subagent,
};

pub(super) const HOOK_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "Notification",
    "Stop",
    "SubagentStop",
    "SessionStart",
    "SessionEnd",
    "PreCompact",
];

pub(super) fn agent_kind(id: &str) -> AgentKind {
    AgentKind::from_agent_id(id).unwrap_or(AgentKind::ClaudeCode)
}

pub(super) fn scope_of(id: &str) -> Scope {
    if id == "workspace" {
        Scope::Workspace
    } else {
        Scope::User
    }
}

pub(super) fn concept_of(id: &str) -> Option<Concept> {
    Some(match id {
        "mcp" => Concept::Mcp,
        "skill" => Concept::Skill,
        "command" => Concept::Command,
        "hook" => Concept::Hook,
        "subagent" => Concept::Subagent,
        _ => return None,
    })
}

pub(super) fn transport_id(t: McpTransport) -> &'static str {
    match t {
        McpTransport::Stdio => "stdio",
        McpTransport::Http => "http",
        McpTransport::Sse => "sse",
    }
}

/// Agent ids of the `agents` form field (comma or newline separated); empty = all agents.
pub(super) fn agent_ids(value: &str) -> Vec<String> {
    value
        .split(['\n', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Form fields of `body`. `overlay` = the Harness MCP overlay target, the only config whose entries know which
/// agent ids receive a server (`McpServer::agents`).
pub(super) fn fields_of(body: &EntryBody, overlay: bool) -> Vec<(String, String)> {
    let f = |k: &str, v: String| (k.to_string(), v);
    match body {
        // Harness overlay only: the CLI configs have no per-agent filter.
        EntryBody::Mcp(s) if overlay => {
            let mut fields = fields_of(body, false);
            fields.push(f("agents", s.agents.join(", ")));
            fields
        }
        EntryBody::Mcp(s) => vec![
            f("name", s.name.clone()),
            f("transport", transport_id(s.transport).into()),
            f("command", opt(s.command.as_ref())),
            f("args", s.args.join("\n")),
            f(
                "env",
                s.env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            f("url", opt(s.url.as_ref())),
            f(
                "headers",
                s.headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        ],
        EntryBody::Skill(s) => vec![
            f("name", s.name.clone()),
            f("description", opt(s.description.as_ref())),
            f("body", s.body.clone()),
        ],
        EntryBody::Command(c) => vec![
            f("name", c.name.clone()),
            f("description", opt(c.description.as_ref())),
            f("argumentHint", opt(c.argument_hint.as_ref())),
            f("model", opt(c.model.as_ref())),
            f("body", c.body.clone()),
        ],
        EntryBody::Hook(h) => vec![
            f("event", h.event.clone()),
            f("matcher", opt(h.matcher.as_ref())),
            f("command", h.command.clone()),
            f(
                "timeout",
                h.timeout.map(|t| t.to_string()).unwrap_or_default(),
            ),
            f("statusMessage", opt(h.status_message.as_ref())),
            f("async", h.run_async.unwrap_or(false).to_string()),
        ],
        EntryBody::Subagent(s) => vec![
            f("name", s.name.clone()),
            f("description", opt(s.description.as_ref())),
            f("tools", opt(s.tools.as_ref())),
            f("model", opt(s.model.as_ref())),
            f("color", opt(s.color.as_ref())),
            f("systemPrompt", s.body.clone()),
        ],
        EntryBody::Instructions { .. } => Vec::new(),
    }
}

pub(super) fn default_body(concept: Concept) -> EntryBody {
    match concept {
        Concept::Mcp => EntryBody::Mcp(McpServer::default()),
        Concept::Skill => EntryBody::Skill(Skill::default()),
        Concept::Command => EntryBody::Command(CmdEntry::default()),
        Concept::Hook => EntryBody::Hook(Hook {
            event: "PreToolUse".into(),
            hook_type: "command".into(),
            ..Hook::default()
        }),
        Concept::Subagent | Concept::Instructions => EntryBody::Subagent(Subagent::default()),
    }
}

pub(super) fn body_from(base: EntryBody, fields: &[(String, String)], overlay: bool) -> EntryBody {
    let get = |k: &str| {
        fields
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    match base {
        EntryBody::Mcp(mut s) => {
            s.name = get("name").trim().to_string();
            s.transport = match get("transport").as_str() {
                "http" => McpTransport::Http,
                "sse" => McpTransport::Sse,
                _ => McpTransport::Stdio,
            };
            s.command = non_empty(&get("command"));
            s.args = lines_of(&get("args"));
            s.env = pairs(&get("env"), '=');
            s.url = non_empty(&get("url"));
            s.headers = pairs(&get("headers"), ':');
            // Harness overlay only: keep `agents` untouched for the CLI configs, which never write it.
            if overlay {
                s.agents = agent_ids(&get("agents"));
            }
            EntryBody::Mcp(s)
        }
        EntryBody::Skill(mut s) => {
            s.name = get("name").trim().to_string();
            s.description = non_empty(&get("description"));
            s.body = get("body");
            EntryBody::Skill(s)
        }
        EntryBody::Command(mut c) => {
            c.name = get("name").trim().to_string();
            c.description = non_empty(&get("description"));
            c.argument_hint = non_empty(&get("argumentHint"));
            c.model = non_empty(&get("model"));
            c.body = get("body");
            EntryBody::Command(c)
        }
        EntryBody::Hook(mut h) => {
            h.event = get("event");
            h.matcher = non_empty(&get("matcher"));
            h.command = get("command");
            h.timeout = get("timeout").trim().parse::<f64>().ok();
            h.status_message = non_empty(&get("statusMessage"));
            h.run_async = (get("async") == "true").then_some(true);
            if h.hook_type.is_empty() {
                h.hook_type = "command".into();
            }
            EntryBody::Hook(h)
        }
        EntryBody::Subagent(mut s) => {
            s.name = get("name").trim().to_string();
            s.description = non_empty(&get("description"));
            s.tools = non_empty(&get("tools"));
            s.model = non_empty(&get("model"));
            s.color = non_empty(&get("color"));
            s.body = get("systemPrompt");
            EntryBody::Subagent(s)
        }
        other @ EntryBody::Instructions { .. } => other,
    }
}
