use super::*;

#[test]
fn diff_has_hunks_with_context() {
    let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
    let after = "a\nb\nc\nd\nE\nf\ng\nh\ni\nj\nk\n";
    let d = unified_diff("x", before, after);
    assert!(d.contains("@@ -2,9 +2,10 @@"), "{d}");
    assert!(d.contains("-e\n+E\n"));
    assert!(d.contains("+k\n"));
    assert_eq!(unified_diff("x", before, before), "");
    let created = unified_diff("x", "", "one\ntwo\n");
    assert!(created.contains("@@ -0,0 +1,2 @@"), "{created}");
}

#[test]
fn toml_preview_masks_secrets_keeps_comments() {
    let text = "# keep me\n[mcp_servers.gh]\ncommand = \"gh\"\nbearer_token = \"TOK123\"\nenv = { GH = \"SEC456\" }\n[mcp_servers.gh.http_headers]\nX = \"HDR789\"\n";
    let p = preview(text, Kind::Toml);
    assert!(p.contains("# keep me"));
    for s in ["TOK123", "SEC456", "HDR789"] {
        assert!(!p.contains(s), "{p}");
    }
    assert!(p.contains("command = \"gh\""));
}

#[tokio::test]
async fn apply_detects_conflicts_and_skips_no_ops() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("f.json");
    std::fs::write(&path, "{}")?;
    let plan = WritePlan::single(FileChange::write(
        path.clone(),
        Some("{}".into()),
        "{\"a\":1}".into(),
        Kind::Json,
    ));
    std::fs::write(&path, "{\"b\":2}")?;
    assert!(matches!(
        apply(&plan).await,
        Err(ConfigError::Conflict { .. })
    ));
    assert_eq!(std::fs::read_to_string(&path)?, "{\"b\":2}");
    let same = WritePlan::single(FileChange::write(
        path.clone(),
        Some("{\"b\":2}".into()),
        "{\"b\":2}".into(),
        Kind::Json,
    ));
    assert!(same.is_no_op());
    assert_eq!(apply(&same).await?.written, Vec::<PathBuf>::new());
    Ok(())
}
