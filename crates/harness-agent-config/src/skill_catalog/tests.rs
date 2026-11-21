use super::*;

const INDEX: &str = r#"{
    "generator": "generated",
    "skills": [
        {"name": "data-manager-api-setup", "description": "Guides setup.", "entrypoint": "https://raw.githubusercontent.com/google/skills/main/skills/ads/data-manager-api-setup/SKILL.md"},
        {"name": "../evil", "description": "x", "entrypoint": "https://example.com/skills/ads/evil/SKILL.md"},
        {"name": "plain", "description": "x", "entrypoint": "ftp://example.com/skills/ads/plain/SKILL.md"},
        {"name": "notmd", "description": "x", "entrypoint": "https://example.com/skills/ads/notmd/SKILL.txt"},
        {"name": "cloud-run", "description": "Deploys.", "entrypoint": "https://raw.githubusercontent.com/google/skills/main/skills/cloud/cloud-run/SKILL.md"}
    ]
}"#;

#[test]
fn catalog_skips_unusable_entries() {
    let catalog = parse_catalog(INDEX.as_bytes()).ok().unwrap_or_default();
    let names: Vec<&str> = catalog.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["cloud-run", "data-manager-api-setup"]);
    assert_eq!(catalog.skills[0].group, "cloud");
    assert_eq!(catalog.skills[1].group, "ads");
    assert_eq!(catalog.skills[1].description, "Guides setup.");
}

#[test]
fn catalog_rejects_non_json() {
    assert!(parse_catalog(b"not json").is_err());
    assert!(parse_catalog(br#"{"nope": []}"#).is_err());
}

#[test]
fn tree_and_extra_files() {
    let tree = br#"{"truncated": false, "tree": [
        {"path": "skills/ads/setup", "type": "tree", "sha": "t"},
        {"path": "skills/ads/setup/SKILL.md", "type": "blob", "sha": "a"},
        {"path": "skills/ads/setup/references/java.md", "type": "blob", "sha": "b"},
        {"path": "skills/ads/setup/../../escape.md", "type": "blob", "sha": "c"},
        {"path": "skills/ads/other/SKILL.md", "type": "blob", "sha": "d"}
    ]}"#;
    let entries = parse_tree(tree).expect("parse");
    assert_eq!(entries.len(), 5);
    let files = skill_files(&entries, "ads", "setup");
    assert_eq!(
        files,
        vec![("references/java.md".to_owned(), "b".to_owned())]
    );
    assert!(skill_files(&entries, "ads", "missing").is_empty());
}

#[test]
fn safe_relative_refuses_escapes() {
    for ok in ["references/java.md", "assets/a b.png", ".hidden"] {
        assert!(safe_relative(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "/etc/passwd",
        "C:/x.md",
        "a\\b.md",
        "../evil.md",
        "a/../../evil.md",
        "a//b.md",
        "a/./b.md",
        "a/\u{1}b.md",
    ] {
        assert!(safe_relative(bad).is_err(), "{bad}");
    }
}

#[test]
fn skill_md_reads_block_scalars() {
    let text = "---\nname: data-manager-api-setup\ndescription: >-\n  Guides developers through the setup.\n  Use this skill when the user asks.\nmetadata:\n  version: \"1.1.1\"\n  category: GoogleA\n---\n# Title\nBody\n";
    let skill = parse_skill_md("data-manager-api-setup", text);
    assert_eq!(
        skill.description.as_deref(),
        Some("Guides developers through the setup. Use this skill when the user asks.")
    );
    assert_eq!(skill.body, "# Title\nBody\n");
    assert!(skill
        .extra_front_matter
        .iter()
        .any(|(k, v)| k == "metadata" && v.contains("version")));
}

#[test]
fn skill_md_handles_plain_and_literal_scalars() {
    let plain = parse_skill_md(
        "s",
        "---\nname: s\ndescription: \"Short: quoted\"\n---\nBody\n",
    );
    assert_eq!(plain.description.as_deref(), Some("Short: quoted"));
    let literal = parse_skill_md("s", "---\ndescription: |\n  line one\n  line two\n---\n");
    assert_eq!(literal.description.as_deref(), Some("line one\nline two"));
    let none = parse_skill_md("s", "Body only\n");
    assert_eq!(none.description, None);
}

#[tokio::test]
async fn install_plan_writes_skill_and_extra_files() -> Result<()> {
    let tmp = tempfile::tempdir().map_err(ConfigError::Io)?;
    let skill = parse_skill_md(
        "cloud-run",
        "---\nname: cloud-run\ndescription: >-\n  Deploys to Cloud Run.\n---\n# Cloud Run\n",
    );
    let files = vec![
        ("references/java.md".to_owned(), "java\n".to_owned()),
        ("SKILL.md".to_owned(), "ignored\n".to_owned()),
    ];
    let files: Vec<(String, String)> = files
        .into_iter()
        .filter(|(rel, _)| rel != SKILL_FILE)
        .collect();
    let plan = plan_install(tmp.path(), &skill, &files).await?;
    assert_eq!(plan.changes.len(), 2);
    assert!(plan
        .changes
        .iter()
        .any(|c| c.path.ends_with("cloud-run/SKILL.md")));
    assert!(plan
        .changes
        .iter()
        .any(|c| c.path.ends_with("cloud-run/references/java.md")));

    crate::plan::apply(&plan).await?;
    let installed = fsx::read_to_string_opt(&tmp.path().join("cloud-run/SKILL.md"))
        .await
        .map_err(ConfigError::Io)?;
    let installed = installed.expect("SKILL.md written");
    assert!(installed.contains("description: Deploys to Cloud Run."));
    assert!(installed.contains("name: cloud-run"));

    // second install is a no-op (nothing duplicated, no diff)
    let again = plan_install(tmp.path(), &skill, &files).await?;
    assert!(again.is_no_op(), "second install must be a no-op");
    Ok(())
}

#[tokio::test]
async fn install_plan_refuses_escaping_extra_file() -> Result<()> {
    let tmp = tempfile::tempdir().map_err(ConfigError::Io)?;
    let skill = parse_skill_md("s", "---\nname: s\n---\nBody\n");
    let files = vec![("../escape.md".to_owned(), "x".to_owned())];
    assert!(plan_install(tmp.path(), &skill, &files).await.is_err());
    assert!(!tmp.path().parent().unwrap().join("escape.md").exists());
    Ok(())
}
