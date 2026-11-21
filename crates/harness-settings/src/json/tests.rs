use super::*;

#[test]
fn keeps_order_and_indent() -> Result<(), serde_json::Error> {
    let text = "{\n    \"zeta\": 1,\n    \"alpha\": {\n        \"b\": true,\n        \"a\": null\n    }\n}\n";
    let mut value = OValue::parse(text)?;
    assert_eq!(detect_indent(text), "    ");
    assert_eq!(value.to_pretty(&detect_indent(text), true), text);
    if let Some(map) = value.as_object_mut() {
        map.object_mut("alpha").insert("b", OValue::Bool(false));
        map.insert("new", OValue::String("x".into()));
    }
    let out = value.to_pretty("    ", true);
    assert!(out.find("zeta") < out.find("alpha"));
    assert!(out.find("\"b\": false") < out.find("\"a\""));
    assert!(out.trim_end().ends_with("\"new\": \"x\"\n}"));
    Ok(())
}

#[test]
fn equality_ignores_key_order() -> Result<(), serde_json::Error> {
    assert_eq!(
        OValue::parse(r#"{"a":1,"b":[1,{"c":2,"d":3}]}"#)?,
        OValue::parse(r#"{"b":[1,{"d":3,"c":2}],"a":1}"#)?
    );
    assert_ne!(OValue::parse(r#"{"a":1}"#)?, OValue::parse(r#"{"a":2}"#)?);
    Ok(())
}

#[test]
fn merge_keeps_unknown_and_replaces_paths() -> Result<(), serde_json::Error> {
    let mut dst =
        OValue::parse(r#"{"x":1,"general":{"lang":"en","unknown":true},"shortcuts":{"a":"1"}}"#)?;
    let src = OValue::parse(r#"{"general":{"lang":"zh"},"shortcuts":{"b":"2"}}"#)?;
    dst.merge_from(&src, &["shortcuts"]);
    assert_eq!(
        dst,
        OValue::parse(r#"{"x":1,"general":{"lang":"zh","unknown":true},"shortcuts":{"b":"2"}}"#)?
    );
    Ok(())
}

#[test]
fn masking_hides_secrets() -> Result<(), serde_json::Error> {
    let value = OValue::parse(
        r#"{"mcpServers":{"s":{"command":"x","env":{"GITHUB":"SECRET1"},"headers":{"X":"SECRET2"}}},"primaryApiKey":"SECRET3"}"#,
    )?;
    let text = masked(&value, &["env", "headers"]).to_pretty("  ", false);
    for secret in ["SECRET1", "SECRET2", "SECRET3"] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    assert!(text.contains("\"command\": \"x\""));
    assert_eq!(mask_secret("a"), mask_secret("a"));
    Ok(())
}
