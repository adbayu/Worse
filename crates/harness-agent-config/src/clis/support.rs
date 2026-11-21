use super::{AgentKind, Concept, ConceptSupport, McpTransport};

// ================================================================================================ support table

pub(super) fn row(
    concept: Concept,
    user: bool,
    workspace: bool,
    can_disable: bool,
    reason: Option<&str>,
) -> ConceptSupport {
    ConceptSupport {
        concept,
        user,
        workspace,
        can_disable,
        transports: Vec::new(),
        reason: reason.map(str::to_owned),
    }
}

/// What `agent` supports for `concept`.
#[must_use]
pub fn support_of(agent: AgentKind, concept: Concept) -> ConceptSupport {
    use McpTransport::{Http, Sse, Stdio};
    let none = |why: &str| row(concept, false, false, false, Some(why));
    match (agent, concept) {
        (AgentKind::ClaudeCode, Concept::Mcp) => ConceptSupport {
            transports: vec![Stdio, Http, Sse],
            ..row(
                concept,
                true,
                true,
                true,
                Some("on/off applies to workspace (.mcp.json) servers only"),
            )
        },
        (AgentKind::ClaudeCode, Concept::Skill | Concept::Subagent) => {
            row(concept, true, true, false, None)
        }
        (AgentKind::ClaudeCode, Concept::Command | Concept::Hook | Concept::Instructions) => {
            row(concept, true, true, false, None)
        }
        (AgentKind::Codex, Concept::Mcp) => ConceptSupport {
            transports: vec![Stdio, Http],
            ..row(concept, true, true, true, None)
        },
        (AgentKind::Codex, Concept::Skill) => row(
            concept,
            true,
            true,
            false,
            Some("workspace skills (.codex/skills) are unverified"),
        ),
        (AgentKind::Codex, Concept::Command) => row(
            concept,
            true,
            false,
            false,
            Some("Codex custom prompts are user-scope only"),
        ),
        (AgentKind::Codex, Concept::Instructions) => row(concept, true, true, false, None),
        (AgentKind::Codex, Concept::Hook) => {
            none("Codex hook format is not verified; not supported")
        }
        (AgentKind::Codex, Concept::Subagent) => none("Codex has no subagent definitions"),
        (AgentKind::Agy, Concept::Mcp) => ConceptSupport {
            transports: vec![Stdio, Http],
            ..row(
                concept,
                true,
                true,
                true,
                Some("agy mcp_config.json format is unverified"),
            )
        },
        (AgentKind::Agy, Concept::Instructions) => row(concept, true, true, false, None),
        (AgentKind::Agy, _) => none("agy has no such configuration that Harness can verify"),
        (AgentKind::Harness, Concept::Mcp) => ConceptSupport {
            transports: vec![Stdio, Http, Sse],
            ..row(
                concept,
                true,
                false,
                true,
                Some("the Harness overlay is user scope only"),
            )
        },
        (AgentKind::Harness, Concept::Skill) => row(
            concept,
            true,
            false,
            false,
            Some("Harness skills are user scope only"),
        ),
        (AgentKind::Harness, _) => none("Harness holds its own MCP servers and skills only"),
    }
}
