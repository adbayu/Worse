use super::*;
use crate::auth::StaticProbe;

#[tokio::test]
async fn copies_skill_folder_to_harness() -> Result<()> {
    let home = tempfile::tempdir()?;
    let data = tempfile::tempdir()?;
    let skill = home.path().join(".claude").join("skills").join("graph");
    std::fs::create_dir_all(skill.join("references"))?;
    std::fs::create_dir_all(skill.join(".cache"))?;
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: graph\ndescription: Graph\n---\nBody\n",
    )?;
    std::fs::write(skill.join("references").join("api.md"), "ref text\n")?;
    std::fs::write(skill.join("logo.bin"), [0xff_u8, 0xfe, 0x00])?;
    std::fs::write(skill.join(".cache").join("x.md"), "hidden\n")?;
    let paths = ConfigPaths::for_home(home.path(), data.path(), None);
    let svc = AgentConfigService::new(paths, Arc::new(StaticProbe::default()));
    let from = ConfigTarget {
        agent: AgentKind::ClaudeCode,
        scope: Scope::User,
    };
    let to = ConfigTarget {
        agent: AgentKind::Harness,
        scope: Scope::User,
    };
    let plan = svc.plan_copy(Concept::Skill, from, to, "graph").await?;
    svc.apply(&plan).await?;
    let target = data.path().join("skills").join("graph");
    assert!(std::fs::read_to_string(target.join("SKILL.md"))?.contains("Body"));
    assert_eq!(
        std::fs::read_to_string(target.join("references").join("api.md"))?,
        "ref text\n"
    );
    assert!(!target.join("logo.bin").exists());
    assert!(!target.join(".cache").exists());
    let listed = svc.list(Concept::Skill, to).await?;
    assert_eq!(listed.len(), 1);
    Ok(())
}
