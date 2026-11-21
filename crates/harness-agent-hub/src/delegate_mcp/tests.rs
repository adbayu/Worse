#[test]
fn every_tool_is_always_loaded_for_agy() {
    for tools in [
        super::tool_list(false),
        super::tool_list(true),
        super::router_tools(),
    ] {
        let tools = super::always_loaded(tools);
        for tool in tools.as_array().into_iter().flatten() {
            assert_eq!(tool["_meta"]["antigravity/alwaysLoad"], true, "{tool}");
        }
    }
}

#[test]
fn a_routed_delegation_says_who_picked_and_why() {
    use serde_json::json;
    let routed = json!({"routedBy": {"decisionModel": true, "agent": "x", "model": "worker/strong", "reason": "review work"}});
    assert_eq!(
        super::routed_note(&routed),
        "\n\n[Picked by the decision model, model worker/strong: review work]"
    );
    assert_eq!(super::routed_note(&json!({"sessionId": "s"})), "");
}
