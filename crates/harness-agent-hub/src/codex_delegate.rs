//! Keeps Codex orchestrator sessions on Harness-managed Orchestra delegation.

use harness_agent_config::AgentKind;
use harness_core_api::{AgentId, Orchestra, SessionId};

use crate::hub::HubCtx;

const HINT: &str = "<harness-subagents>\n\
When the user asks to spawn, use or deploy subagents, delegate work, split work across agents, or work in parallel, use the Harness `harness-delegate` MCP tools. Call `list_agents`, then `delegate` with a complete task and an agent id from that list.\n\
Do not use Codex's built-in subagent or `spawn_agent` tools for this: they create Codex-native children outside the user's Harness Orchestra. If `harness-delegate` or `delegate` is unavailable or returns an error, report that exact error; do not silently do the delegated task yourself.\n\
</harness-subagents>";

/// Identifies whether `agent` is a Codex agent.
pub(crate) fn is_codex(ctx: &HubCtx, agent: &AgentId) -> bool {
    agent.as_str() == "codex" || ctx.config.agent_kinds.get(agent) == Some(&AgentKind::Codex)
}

/// Adds explicit Harness delegation instructions to Codex orchestrator prompts.
pub(crate) async fn hint(
    ctx: &HubCtx,
    agent: &AgentId,
    session: &SessionId,
) -> Option<&'static str> {
    if !is_codex(ctx, agent)
        || ctx.subagents.endpoint.get().is_none()
        || ctx.subagents.is_child(session)
        || ctx.routers.is_router(session)
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
