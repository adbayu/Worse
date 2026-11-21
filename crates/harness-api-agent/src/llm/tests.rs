use super::*;

fn messages() -> Vec<Value> {
    vec![
        json!({"role": "system", "content": "be brief"}),
        json!({"role": "user", "content": "hi"}),
        json!({"role": "assistant", "content": ""}),
    ]
}

#[test]
fn system_message_gets_breakpoint() {
    let out = with_cache_breakpoints(&messages());
    assert_eq!(
        out[0]["content"],
        json!([{"type": "text", "text": "be brief", "cache_control": {"type": "ephemeral"}}])
    );
}

#[test]
fn exactly_one_non_system_breakpoint() {
    let out = with_cache_breakpoints(&messages());
    let marked = out
        .iter()
        .skip(1)
        .filter(|m| m.to_string().contains("cache_control"))
        .count();
    assert_eq!(marked, 1);
    // The empty assistant message is skipped: the user message before it holds the breakpoint.
    assert_eq!(
        out[1]["content"],
        json!([{"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}])
    );
}

#[test]
fn array_content_marks_last_part() {
    let out = with_cache_breakpoints(&[
        json!({"role": "user", "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}),
        json!({"role": "assistant", "content": null}),
    ]);
    assert!(out[0]["content"][0].get("cache_control").is_none());
    assert_eq!(
        out[0]["content"][1]["cache_control"],
        json!({"type": "ephemeral"})
    );
}

#[test]
fn input_slice_is_unchanged() {
    let input = messages();
    let before = json!(input);
    let _ = with_cache_breakpoints(&input);
    assert_eq!(json!(input), before);
}

#[test]
fn cache_tokens_parses_provider_spellings() {
    assert_eq!(
        cache_tokens(&json!({"prompt_tokens_details": {"cached_tokens": 12}})),
        (12, 0)
    );
    assert_eq!(
        cache_tokens(&json!({"prompt_cache_hit_tokens": 34})),
        (34, 0)
    );
    assert_eq!(
        cache_tokens(&json!({"cache_read_input_tokens": 56, "cache_creation_input_tokens": 7})),
        (56, 7)
    );
    assert_eq!(cache_tokens(&json!({"prompt_tokens": 3})), (0, 0));
}

#[test]
fn images_strip_to_plain_text() {
    let m = vec![json!({"role": "user", "content": [
        {"type": "text", "text": "look"}, image_part("image/png", "AAAA")]})];
    assert!(has_images(&m));
    let out = strip_images(&m);
    assert!(!has_images(&out));
    assert!(out[0]["content"]
        .as_str()
        .is_some_and(|c| c.starts_with("look") && c.contains("cannot view")));
    assert!(has_images(&m));
}

#[test]
fn prune_keeps_newest_images() {
    let img = |t: &str| json!({"role": "user", "content": [{"type": "text", "text": t}, image_part("image/png", "AAAA")]});
    let mut m = vec![img("a"), img("b"), img("c")];
    prune_images(&mut m, 2);
    assert!(m[0]["content"].is_string());
    assert!(m[1]["content"].is_array() && m[2]["content"].is_array());
}

#[test]
fn image_size_is_fixed_cost() {
    let big = "A".repeat(500_000);
    let m = json!({"role": "user", "content": [image_part("image/png", &big)]});
    assert!(message_size(&m) < 5_000);
}

#[test]
fn rejection_needs_image_hint() {
    assert!(image_rejection(
        400,
        "unknown variant `image_url`, expected `text`"
    ));
    assert!(!image_rejection(400, "cache_control is not supported"));
    assert!(!image_rejection(500, "image"));
}
