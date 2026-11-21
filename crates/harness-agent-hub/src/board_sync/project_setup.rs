use super::{
    ensure_status_field, graphql, json, now_ms, split_repo, text, Link, CREATE_PROJECT,
    DELETE_WORKFLOW, FIGHTING_WORKFLOWS, REPO_QUERY, WORKFLOWS,
};

async fn remove_fighting_workflows(project_id: &str) -> Result<(), String> {
    let data = graphql(WORKFLOWS, json!({ "p": project_id })).await?;
    let nodes = data["node"]["workflows"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for node in nodes
        .iter()
        .filter(|n| FIGHTING_WORKFLOWS.contains(&n["name"].as_str().unwrap_or_default()))
    {
        graphql(DELETE_WORKFLOW, json!({ "w": node["id"] }))
            .await
            .map_err(|e| {
                format!(
                    "could not turn off the \"{}\" project workflow: {e}",
                    text(&node["name"])
                )
            })?;
    }
    Ok(())
}

pub(super) async fn create_link(repo: &str) -> Result<Link, String> {
    let (owner, name) = split_repo(repo)?;
    let data = graphql(REPO_QUERY, json!({ "o": owner, "r": name })).await?;
    let repository = &data["repository"];
    if repository.is_null() {
        return Err(format!("repository {repo} not found or not visible to gh"));
    }
    let existing = repository["projectsV2"]["nodes"]
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|p| p["closed"].as_bool() != Some(true))
                .cloned()
        });
    let project = if let Some(project) = existing {
        project
    } else {
        let created = graphql(
            CREATE_PROJECT,
            json!({
                "o": repository["owner"]["id"],
                "r": repository["id"],
                "t": format!("{} board", text(&repository["name"])),
            }),
        )
        .await?;
        created["createProjectV2"]["projectV2"].clone()
    };
    let project_id = text(&project["id"]);
    if project_id.is_empty() {
        return Err("GitHub did not return a project".to_owned());
    }
    let (status_field_id, options) = ensure_status_field(&project_id).await?;
    let workflow_error = remove_fighting_workflows(&project_id).await.err();
    Ok(Link {
        repo: repo.to_owned(),
        project_id,
        project_number: project["number"].as_u64().unwrap_or_default(),
        project_url: text(&project["url"]),
        project_title: text(&project["title"]),
        status_field_id,
        options,
        linked_at: now_ms(),
        last_sync: None,
        error: None,
        workflow_error,
    })
}
