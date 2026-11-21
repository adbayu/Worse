use super::{json, Value};

/// Marks every tool `antigravity/alwaysLoad` so agy declares it up front instead of loading it on demand
/// (agy lists on-demand tools only through a lookup it does not offer over ACP).
pub(super) fn always_loaded(mut tools: Value) -> Value {
    for tool in tools.as_array_mut().into_iter().flatten() {
        tool["_meta"] = json!({"antigravity/alwaysLoad": true});
    }
    tools
}

fn claim_tools() -> [Value; 2] {
    [
        json!({
            "name": "claim_paths",
            "description": "Claim files or folders (relative to the workspace) before editing them, so parallel agents do not edit the same files. Returns which paths were granted, which are already held by another agent (with its session, title and note), and every current claim. Do not edit a path you could not claim; coordinate instead. Call with no paths to only list claims. Your claims are released automatically when your turn ends.",
            "inputSchema": {"type": "object", "properties": {
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Workspace-relative files or folders, e.g. src/app.rs or crates/hub."},
                "note": {"type": "string", "description": "What you are changing there (shown to other agents)."}
            }}
        }),
        json!({
            "name": "release_paths",
            "description": "Release paths you claimed (all of them when paths is omitted) as soon as you are done editing them.",
            "inputSchema": {"type": "object", "properties": {
                "paths": {"type": "array", "items": {"type": "string"}}
            }}
        }),
    ]
}

pub(super) fn router_tools() -> Value {
    json!([
        {
            "name": "list_agents",
            "description": "List the Orchestra team you can route requests to (id, name, state, role).",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "route_trigger",
            "description": "Hand one inbox request to an agent of the team. Harness opens a ticket with its own git worktree for that agent and adds your brief to the agent's first message.",
            "inputSchema": {"type": "object", "properties": {
                "trigger_id": {"type": "string", "description": "The request id, e.g. comment:123 or issue:456."},
                "agent": {"type": "string", "description": "Agent id from list_agents (or your own id to take it yourself)."},
                "brief": {"type": "string", "description": "Short notes for the agent: what to do and where to look."}
            }, "required": ["trigger_id", "agent"]}
        },
        {
            "name": "dismiss_trigger",
            "description": "Close one inbox request without work (spam, duplicate, already answered). Nothing is posted to GitHub.",
            "inputSchema": {"type": "object", "properties": {
                "trigger_id": {"type": "string", "description": "The request id."},
                "reason": {"type": "string", "description": "One short sentence shown in the Harness trigger log."}
            }, "required": ["trigger_id", "reason"]}
        }
    ])
}

/// Delegate tools of a parent plus `notify_parent` from the subagent list.
pub(crate) fn nested_tools(parent: &Value, child: &Value) -> Value {
    let mut tools = parent.as_array().cloned().unwrap_or_default();
    if let Some(notify) = child
        .as_array()
        .and_then(|list| list.iter().find(|t| t["name"] == "notify_parent"))
    {
        tools.push(notify.clone());
    }
    Value::Array(tools)
}

pub(crate) fn tool_list(child: bool) -> Value {
    let [claim, release] = claim_tools();
    if child {
        return json!([
            {
                "name": "notify_parent",
                "description": "Send a short note to the agent that delegated your task, without ending your turn: progress, a question, a blocker, or a conflict on claimed paths. It is delivered when the parent next waits on its subagents.",
                "inputSchema": {"type": "object", "properties": {
                    "text": {"type": "string", "description": "The note (at most 4000 characters)."}
                }, "required": ["text"]}
            },
            claim,
            release
        ]);
    }
    json!([
        {
            "name": "list_agents",
            "description": "List the AI coding agents Harness can delegate to (id, name, state). Use an id as the `agent` of delegate.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "delegate",
            "description": "Hand a self-contained task to another AI coding agent managed by Harness (for example Codex, another Claude account, or an API model such as DeepSeek). Use this, not a built-in subagent/Task/Agent tool, whenever the user asks for a subagent. The subagent works in the same project folder in its own Harness session, which the user can watch live. The subagent does not see this conversation: put every needed detail (files, goal, constraints, expected output) in `task`. By default this call waits and returns the final answer. With `background: true it returns at once with the subagent's session id, so you can keep working or end your turn and send it more instructions with subagent_message. When it finishes, Harness delivers its answer to you automatically as a new message, so do not poll with subagent_wait; stopping your own turn does not stop the subagent. Several subagents can run at once (the user sets the limit in Harness settings). To send a follow-up to the same subagent, pass the session_id from its earlier result. Split work so subagents edit different files; they claim paths with claim_paths.",
            "inputSchema": {"type": "object", "properties": {
                "task": {"type": "string", "description": "Complete instructions for the subagent."},
                "agent": {"type": "string", "description": "Agent id from list_agents. Omit to let Harness pick: the user's decision model chooses the agent (and model) that suits the task when one is set, otherwise the first available agent."},
                "title": {"type": "string", "description": "Short label shown in the Harness subagent panel."},
                "session_id": {"type": "string", "description": "Continue an earlier subagent session instead of starting a new one."},
                "background": {"type": "boolean", "description": "Return immediately; the answer is delivered to you as a new message when the subagent finishes."},
                "scope": {"type": "object", "description": "Optional narrowing of the subagent's permissions; a wider scope than your own is rejected.", "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Workspace-relative paths or folders it may touch; omit for the whole workspace."},
                    "tools": {"type": "array", "items": {"type": "string"}, "description": "Tool name fragments it may use; omit for every tool."},
                    "network": {"type": "boolean", "description": "Whether network tools are allowed (default true, and it cannot be wider than your own scope)."},
                    "minutes": {"type": "integer", "description": "Minutes until its permissions expire."}
                }}
            }, "required": ["task"]}
        },
        {
            "name": "subagent_message",
            "description": "Send new instructions to one of your subagents while it runs (or after it finished). The message is queued as its next turn; with interrupt: true its current turn is stopped first and the message is sent right away. Returns immediately; the reply is delivered to you as a new message.",
            "inputSchema": {"type": "object", "properties": {
                "session_id": {"type": "string", "description": "The subagent's session id."},
                "text": {"type": "string", "description": "The message."},
                "interrupt": {"type": "boolean", "description": "Stop the current turn first (default false)."}
            }, "required": ["session_id", "text"]}
        },
        {
            "name": "subagent_wait",
            "description": "Wait until one of your subagents finishes a turn or sends a note (notify_parent), then return every subagent's status, the unread notes and the full answers of newly finished turns. Only needed when you must have a result inside the current turn: finished answers are otherwise delivered to you automatically as a new message. `timeout_s: 0 returns the status immediately.",
            "inputSchema": {"type": "object", "properties": {
                "session_ids": {"type": "array", "items": {"type": "string"}, "description": "Only these subagents (default: all of yours)."},
                "timeout_s": {"type": "integer", "description": "Seconds to wait (default 300, at most 3600)."}
            }}
        },
        claim,
        release
    ])
}
