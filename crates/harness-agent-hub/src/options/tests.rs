use super::*;

#[test]
fn parse_and_validate() {
    let mut adv = Advertised::default();
    let changed = adv.merge_response(&json!({
        "sessionId": "x",
        "modes": {"currentModeId": "default", "availableModes": [{"id": "default", "name": "D"}, {"id": "plan", "name": "P"}]},
        "models": {"currentModelId": "m1", "availableModels": [{"modelId": "m1", "name": "M1"}]},
        "configOptions": [{"id": "effort", "name": "E", "currentValue": "low", "options": [{"value": "low", "name": "low"}, {"value": "high", "name": "high"}]}]
    }));
    assert!(changed);
    assert!(!adv.clone().merge_response(&json!({"sessionId": "x", "modes": {"currentModeId": "default", "availableModes": [{"id": "default", "name": "D"}, {"id": "plan", "name": "P"}]}})));
    assert!(PrefChange::Mode("plan".into()).validate(&adv).is_ok());
    assert!(PrefChange::Mode("nope".into()).validate(&adv).is_err());
    assert!(PrefChange::Config("effort".into(), "high".into())
        .validate(&adv)
        .is_ok());
    assert!(PrefChange::Config("effort".into(), "max".into())
        .validate(&adv)
        .is_err());
    assert!(
        adv.merge_update(&json!({"sessionUpdate": "current_mode_update", "currentModeId": "plan"}))
    );
    let prefs = Prefs {
        model: Some("m1".into()),
        ..Prefs::default()
    };
    let defaults = AgentDefaults {
        mode: Some("plan".into()),
        ..AgentDefaults::default()
    };
    let target = prefs.over_defaults(Some(&defaults));
    let calls = target.calls_from(&Prefs::default(), "n");
    assert_eq!(calls.len(), 2);
    assert_eq!(target.calls_from(&target, "n").len(), 0);
    let view = config_view(AgentId::new("a"), &adv, &Prefs::default(), None);
    assert_eq!(view.options[0].current.as_deref(), Some("low"));
}
