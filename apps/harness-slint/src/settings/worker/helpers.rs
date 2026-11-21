use super::{
    AgentConfigService, AgentKind, Concept, ConceptSupport, ConfigError, McpImport, State,
    WritePlan,
};
use std::fmt::Write as _;
/// Runs `f` on the UI thread with the settings state.
pub(super) fn post(f: impl FnOnce(&mut State) + Send + 'static) {
    // 事件循环已退出（关停阶段）时丢弃结果是正确行为。
    let _ = slint::invoke_from_event_loop(move || super::super::with_state(f));
}

pub(super) fn message(error: &ConfigError) -> String {
    error.to_string()
}

pub(super) fn plan_paths(plan: &WritePlan) -> String {
    plan.changes
        .iter()
        .map(|c| c.path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// True when `agent` accepts skills at user scope.
pub(super) fn market_supports(svc: &AgentConfigService, agent: AgentKind) -> bool {
    svc.capabilities(agent)
        .into_iter()
        .any(|c: ConceptSupport| c.concept == Concept::Skill && c.user)
}

pub(super) fn import_summary(import: &McpImport) -> String {
    let mut text = format!(
        "Imported {}, skipped {}",
        import.imported.len(),
        import.skipped
    );
    if !import.failed.is_empty() {
        let names = import
            .failed
            .iter()
            .map(|a| match a {
                AgentKind::ClaudeCode => "Claude Code",
                AgentKind::Codex => "Codex",
                AgentKind::Agy => "agy",
                AgentKind::Harness => "Harness",
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(text, " (could not read {names})");
    }
    text
}
