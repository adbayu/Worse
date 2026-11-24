//! Keeps Codex orchestrator sessions on Harness-managed Orchestra delegation.

use harness_core_api::{AgentId, Orchestra, SessionId};

use crate::hub::HubCtx;

const HINT: &str = "<harness-subagents>\n\
When the user asks to spawn, use or deploy subagents, delegate work, split work across agents, or work in parallel, use the Harness `harness-delegate` MCP tools. Call `list_agents`, then `delegate` with a complete task and an agent id from that list.\n\
Do not use Codex's built-in subagent or `spawn_agent` tools for this: they create Codex-native children outside the user's Harness Orchestra. If `harness-delegate` or `delegate` is unavailable or returns an error, report that exact error; do not silently do the delegated task yourself.\n\
</harness-subagents>";

const TRIGGERS: [&str; 12] = [
    "subagent",
    "sub-agent",
    "sub agent",
    "delegate",
    "delegasi",
    "spawn",
    "agent lain",
    "agen lain",
    "other agent",
    "another agent",
    "parallel",
    "paralel",
];

/// Adds explicit Harness delegation instructions to Codex orchestrator prompts.
pub(crate) async fn hint(
    ctx: &HubCtx,
    agent: &AgentId,
    session: &SessionId,
    fresh: bool,
    user_text: &str,
) -> Option<&'static str> {
    if agent.as_str() != "codex"
        || ctx.subagents.endpoint.get().is_none()
        || ctx.subagents.is_child(session)
        || ctx.routers.is_router(session)
        || !(fresh || mentions(user_text))
    {
        return None;
    }

    let raw = ctx
        .store
        .run(|c| crate::store::get_setting(c, "orchestra"))
        .await
        .ok()
        .flatten()?;
    let orchestra = serde_json::from_str::<Orchestra>(&raw).ok()?;
    orchestra
        .orchestrator()
        .is_some_and(|node| &node.agent == agent)
        .then_some(HINT)
}

fn mentions(text: &str) -> bool {
    let text = text.to_lowercase();
    TRIGGERS.iter().any(|word| text.contains(word))
}
