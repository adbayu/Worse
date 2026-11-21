use super::{
    check_segment, fsx, read_dir_sorted, read_listed, ConfigError, Doc, FileChange, Kind, Path,
    PathBuf, Result, Subagent, WritePlan, AGENT_KNOWN,
};

// ----------------------------------------------------------------------------------------------- subagents

pub(crate) async fn list_subagents(dir: &Path) -> Result<Vec<(Subagent, PathBuf)>> {
    let mut out = Vec::new();
    for (file, is_dir) in read_dir_sorted(dir).await? {
        let Some(stem) = file.strip_suffix(".md").filter(|_| !is_dir) else {
            continue;
        };
        let path = dir.join(&file);
        let Some((text, _)) = read_listed(&path).await? else {
            continue;
        };
        let doc = Doc::parse(&text);
        out.push((
            Subagent {
                name: doc.get("name").unwrap_or_else(|| stem.to_owned()),
                description: doc.get("description"),
                tools: doc.get("tools"),
                model: doc.get("model"),
                color: doc.get("color"),
                extra_front_matter: doc.extra(AGENT_KNOWN),
                body: doc.body,
            },
            path,
        ));
    }
    Ok(out)
}

pub(crate) async fn plan_subagent_upsert(
    dir: &Path,
    agent: &Subagent,
    previous_name: Option<&str>,
) -> Result<WritePlan> {
    check_segment(&agent.name)?;
    let existing = list_subagents(dir).await?;
    let lookup = previous_name.unwrap_or(&agent.name);
    let source = existing
        .iter()
        .find(|(a, _)| a.name == lookup)
        .map(|(_, p)| p.clone());
    if previous_name.is_some_and(|p| p != agent.name) {
        if source.is_none() {
            return Err(ConfigError::NotFound(lookup.to_owned()));
        }
        if existing.iter().any(|(a, _)| a.name == agent.name) {
            return Err(ConfigError::invalid(format!(
                "a subagent named {} already exists",
                agent.name
            )));
        }
    }
    let target = match &source {
        Some(p) if previous_name.map_or(true, |prev| prev == agent.name) => p.clone(),
        _ => dir.join(format!("{}.md", agent.name)),
    };
    let base = match &source {
        Some(p) => fsx::read_to_string_opt(p).await?,
        None => None,
    };
    let mut doc = base.as_deref().map(Doc::parse).unwrap_or_default();
    doc.set("name", Some(&agent.name));
    doc.set("description", agent.description.as_deref());
    doc.set("tools", agent.tools.as_deref());
    doc.set("model", agent.model.as_deref());
    doc.set("color", agent.color.as_deref());
    doc.set_extra(AGENT_KNOWN, &agent.extra_front_matter);
    doc.body.clone_from(&agent.body);
    let mut plan = WritePlan::default();
    match source {
        Some(src) if src != target => {
            if fsx::exists(&target).await {
                return Err(ConfigError::invalid(format!(
                    "{} already exists",
                    target.display()
                )));
            }
            plan.changes
                .push(FileChange::write(target, None, doc.render(), Kind::Text));
            plan.changes.push(FileChange::delete(src, base, Kind::Text));
        }
        _ => plan
            .changes
            .push(FileChange::write(target, base, doc.render(), Kind::Text)),
    }
    Ok(plan)
}

pub(crate) async fn plan_subagent_delete(dir: &Path, name: &str) -> Result<WritePlan> {
    let (_, path) = list_subagents(dir)
        .await?
        .into_iter()
        .find(|(a, _)| a.name == name)
        .ok_or_else(|| ConfigError::NotFound(name.to_owned()))?;
    let before = fsx::read_to_string_opt(&path).await?;
    Ok(WritePlan::single(FileChange::delete(
        path,
        before,
        Kind::Text,
    )))
}
