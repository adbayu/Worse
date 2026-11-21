#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::agents::{builtin_agents, ErrorDialect, Transport};

fn builtin(id: &str) -> AgentSpec {
    builtin_agents()
        .into_iter()
        .find(|a| a.id.as_str() == id)
        .unwrap()
}

#[test]
fn code_mode_plan_appends_the_variant_launch() {
    let plan = plan(&builtin("codex"), true);
    assert_eq!(plan.key, (AgentId::new("codex"), true));
    assert_eq!(plan.args, ["-c", "features.code_mode=true"]);
}

#[test]
fn normal_plan_keeps_the_plain_launch() {
    let plan = plan(&builtin("codex"), false);
    assert_eq!(plan.key, (AgentId::new("codex"), false));
    assert_eq!(plan.args.len(), 0);
    assert_eq!(plan.env.len(), 0);
}

#[test]
fn code_mode_falls_back_for_an_agent_without_a_variant() {
    let plan = plan(&builtin("agy"), true);
    assert_eq!(plan.key, (AgentId::new("agy"), false));
    assert_eq!(plan.args, ["acp"]);
}

#[test]
fn api_agent_plan_adds_the_code_mode_switch() {
    let spec = AgentSpec {
        id: AgentId::new("deepseek"),
        display_name: "DeepSeek (API)".into(),
        program: "harness-slint".into(),
        args: vec![crate::helpers::FLAG_API_AGENT.into()],
        env: Vec::new(),
        install_hint: String::new(),
        transport: Transport::Acp,
        dialect: ErrorDialect::Generic,
    };
    let plan = plan(&spec, true);
    assert_eq!(plan.key, (AgentId::new("deepseek"), true));
    assert_eq!(
        plan.env,
        [(
            crate::agents::ENV_HARNESS_CODE_MODE.to_owned(),
            "1".to_owned()
        )]
    );
}

#[test]
fn acquire_rejects_an_unknown_agent() {
    let pool = AgentPool::new(&[], Duration::from_secs(1), EventBus::new(16));
    assert!(pool.plan(&AgentId::new("nope"), true).is_err());
}
