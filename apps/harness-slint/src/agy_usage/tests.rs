use super::*;

#[test]
fn groups_take_the_lowest_remaining_per_family() {
    let models = json!({
        "gemini-3-flash": {"displayName": "G3", "quotaInfo": {"remainingFraction": 0.8}},
        "gemini-4-argon-high": {"displayName": "G4", "quotaInfo": {"remainingFraction": 0.25}},
        "gemini-3.1-flash-image": {"displayName": "Img", "quotaInfo": {"remainingFraction": 0.0}},
        "claude-opus-5-5-high": {"displayName": "Opus", "quotaInfo": {"remainingFraction": 1.0}},
        "chat_1": {"quotaInfo": {"remainingFraction": 0.0}}
    });
    let g = group(&models);
    assert_eq!(g[0].as_ref().map(|x| x.0.round()), Some(20.0));
    assert_eq!(g[1].as_ref().map(|x| x.0), Some(0.0));
    assert!(g[2].is_none());
    assert_eq!(g[3].as_ref().map(|x| x.0.round()), Some(75.0));
}

#[test]
fn requests_carry_the_agy_cli_identity() {
    let ua = user_agent();
    assert!(ua.starts_with("antigravity/cli/1.5.9 (aidev_client;"));
    assert!(ua.contains("auth_method=consumer"));
    if cfg!(all(windows, target_arch = "x86_64")) {
        assert_eq!(platform_enum(), 5);
    }
}

#[test]
fn summary_groups_become_bars() {
    let v = json!({"groups": [{"displayName": "Claude and GPT models", "buckets": [
        {"window": "weekly", "remainingFraction": 0.53},
        {"window": "5h", "remainingFraction": 1},
        {"window": "5h", "disabled": true, "remainingFraction": 0}
    ]}, {"displayName": "Gemini 4 Argon", "buckets": [
        {"displayName": "Weekly Limit", "remainingFraction": 0.8227}
    ]}]});
    let bars = summary_bars(&v);
    assert_eq!(bars.len(), 3);
    assert_eq!(bars[0].0, "Claude & GPT · weekly");
    assert_eq!(bars[0].1.round(), 47.0);
    assert_eq!(bars[2].0, "Gemini 4 Argon · Weekly Limit");
}

#[test]
fn remembered_numbers_show_as_of_and_passed_resets_read_unused() {
    let now = now_secs();
    let bars = vec![
        ("Gemini · weekly".to_owned(), 40.0, Some(now + 3600)),
        ("Claude · 5-hour".to_owned(), 90.0, Some(now - 60)),
    ];
    let row = shown(
        "agy:sek2",
        "Antigravity · Sek2",
        "Pro".into(),
        &bars,
        Some(now - 1500),
    );
    assert_eq!(row.error, "");
    assert_eq!(row.bars[1].1, 0.0);
    assert!(row.bars[0].2.starts_with("Resets in"));
    assert!(row.summary.ends_with("(as of 25m ago)"));
}
