use super::*;

fn stdio(name: &str) -> McpServer {
    McpServer {
        name: name.into(),
        command: Some("npx".into()),
        args: vec!["-y".into(), "pkg".into()],
        ..McpServer::default()
    }
}

#[tokio::test]
async fn codex_toml_keeps_comments_and_unknown_keys() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("config.toml");
    let original = "# my config\nmodel = \"o3\" # inline\n\n[mcp_servers.gh]\n# gh server\ncommand = \"gh\"\nargs = [\"mcp\"]\ncwd = \"/tmp\"\n\n[mcp_servers.gh.env]\nGH_TOKEN = \"SECRET\"\n\n[profiles.x]\nmodel = \"y\"\n";
    std::fs::write(&path, original)?;
    let servers = list_toml(&path).await?;
    assert_eq!(servers.len(), 1);
    let gh = &servers[0];
    assert_eq!(gh.extra.get("cwd").and_then(|v| v.as_str()), Some("/tmp"));
    assert_eq!(gh.env.get("GH_TOKEN").map(String::as_str), Some("SECRET"));
    // Same content → no-op.
    assert!(plan_upsert_toml(&path, gh, None).await?.no_op);
    // Disable + add.
    let change = plan_enabled_toml(&path, "gh", false).await?;
    assert!(!change.diff.contains("SECRET"), "{}", change.diff);
    crate::plan::apply(&crate::plan::WritePlan::single(change)).await?;
    let change = plan_upsert_toml(&path, &stdio("new"), None).await?;
    crate::plan::apply(&crate::plan::WritePlan::single(change)).await?;
    let text = std::fs::read_to_string(&path)?;
    for keep in [
        "# my config",
        "model = \"o3\" # inline",
        "# gh server",
        "cwd = \"/tmp\"",
        "[profiles.x]",
        "enabled = false",
        "[mcp_servers.new]",
    ] {
        assert!(text.contains(keep), "{keep} missing in\n{text}");
    }
    let names: Vec<String> = list_toml(&path)
        .await?
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["gh".to_owned(), "new".to_owned()]);
    Ok(())
}

#[tokio::test]
async fn json_round_trip_keeps_order_and_extra() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".claude.json");
    let original = "{\n  \"numStartups\": 3,\n  \"primaryApiKey\": \"SECRET\",\n  \"mcpServers\": {\n    \"b\": {\"type\": \"stdio\", \"command\": \"x\", \"args\": [], \"env\": {}, \"custom\": 1},\n    \"a\": {\"type\": \"http\", \"url\": \"https://x\"}\n  },\n  \"tail\": true\n}\n";
    std::fs::write(&path, original)?;
    let servers = list_json(&path, Dialect::Claude).await?;
    assert_eq!(
        servers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["b", "a"]
    );
    assert_eq!(servers[1].transport, McpTransport::Http);
    for s in &servers {
        assert!(
            plan_upsert_json(&path, Dialect::Claude, s, None)
                .await?
                .no_op,
            "{}",
            s.name
        );
    }
    let mut b = servers[0].clone();
    b.name = "b2".into();
    let change = plan_upsert_json(&path, Dialect::Claude, &b, Some("b")).await?;
    assert!(!change.diff.contains("SECRET"));
    crate::plan::apply(&crate::plan::WritePlan::single(change)).await?;
    let text = std::fs::read_to_string(&path)?;
    assert!(text.find("numStartups") < text.find("mcpServers"));
    assert!(text.find("\"b2\"") < text.find("\"a\""));
    assert!(text.contains("\"custom\": 1"));
    assert!(text.contains("\"primaryApiKey\": \"SECRET\""));
    assert!(text.trim_end().ends_with("\"tail\": true\n}"));
    Ok(())
}

#[tokio::test]
async fn claude_project_approval() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("settings.local.json");
    let change = plan_claude_project_enabled(&path, "s", true).await?;
    crate::plan::apply(&crate::plan::WritePlan::single(change)).await?;
    assert!(claude_project_enabled(&path).await?("s"));
    let change = plan_claude_project_enabled(&path, "s", false).await?;
    crate::plan::apply(&crate::plan::WritePlan::single(change)).await?;
    assert!(!claude_project_enabled(&path).await?("s"));
    Ok(())
}
