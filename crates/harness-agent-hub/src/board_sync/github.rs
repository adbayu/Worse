use super::{
    aliases, gh, json, lane_color, normalize, BTreeMap, BoardLane, HashSet, Link, Value,
    MAX_BOARD_CARDS, SCOPE_HINT,
};

fn friendly(message: &str) -> String {
    let lower = message.to_lowercase();
    if lower.contains("insufficient_scopes")
        || lower.contains("read:project")
        || lower.contains("'project' scope")
        || lower.contains("project scope")
    {
        return SCOPE_HINT.to_owned();
    }
    message.trim().chars().take(400).collect()
}

pub(super) async fn graphql(query: &str, variables: Value) -> Result<Value, String> {
    let body = json!({ "query": query, "variables": variables }).to_string();
    let out = gh(&["api", "graphql", "--input", "-"], Some(body))
        .await
        .map_err(|e| friendly(&e))?;
    let value: Value =
        serde_json::from_str(&out).map_err(|e| format!("GitHub answered with bad JSON: {e}"))?;
    if let Some(errors) = value["errors"].as_array().filter(|e| !e.is_empty()) {
        let text = errors
            .iter()
            .map(|e| {
                format!(
                    "{} {}",
                    e["type"].as_str().unwrap_or_default(),
                    e["message"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(friendly(&text));
    }
    Ok(value["data"].clone())
}

pub(super) fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

pub(super) const REPO_QUERY: &str = "query($o:String!,$r:String!){ repository(owner:$o,name:$r){ id name owner{ id login } projectsV2(first:20){ nodes{ id number title url closed } } } }";
pub(super) const CREATE_PROJECT: &str = "mutation($o:ID!,$r:ID!,$t:String!){ createProjectV2(input:{ownerId:$o,repositoryId:$r,title:$t}){ projectV2{ id number title url } } }";
const STATUS_FIELD: &str = "query($p:ID!){ node(id:$p){ ... on ProjectV2 { field(name:\"Status\"){ ... on ProjectV2SingleSelectField { id options{ id name color description } } } } } }";
const UPDATE_FIELD: &str = "mutation($f:ID!,$o:[ProjectV2SingleSelectFieldOptionInput!]!){ updateProjectV2Field(input:{fieldId:$f,singleSelectOptions:$o}){ projectV2Field{ ... on ProjectV2SingleSelectField { id options{ id name } } } } }";
const ITEMS: &str = "query($p:ID!,$after:String){ node(id:$p){ ... on ProjectV2 { items(first:100, after:$after){ pageInfo{ hasNextPage endCursor } nodes{ id isArchived fieldValueByName(name:\"Status\"){ ... on ProjectV2ItemFieldSingleSelectValue { optionId } } content{ __typename ... on Issue { number title url body repository{ nameWithOwner } } ... on DraftIssue { title body } } } } } } }";
const SET_STATUS: &str = "mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){ updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,value:{singleSelectOptionId:$o}}){ projectV2Item{ id } } }";
pub(super) const WORKFLOWS: &str =
    "query($p:ID!){ node(id:$p){ ... on ProjectV2 { workflows(first:50){ nodes{ id name } } } } }";
pub(super) const DELETE_WORKFLOW: &str =
    "mutation($w:ID!){ deleteProjectV2Workflow(input:{workflowId:$w}){ clientMutationId } }";
pub(super) const FIGHTING_WORKFLOWS: [&str; 2] =
    ["Pull request linked to issue", "Item added to project"];
pub(super) const CONVERT_DRAFT: &str = "mutation($i:ID!,$r:ID!){ convertProjectV2DraftIssueItemToIssue(input:{itemId:$i,repositoryId:$r}){ item{ id content{ ... on Issue { number url } } } } }";
pub(super) const DELETE_ITEM: &str = "mutation($p:ID!,$i:ID!){ deleteProjectV2Item(input:{projectId:$p,itemId:$i}){ deletedItemId } }";
pub(super) const ADD_ITEM: &str = "mutation($p:ID!,$c:ID!){ addProjectV2ItemById(input:{projectId:$p,contentId:$c}){ item{ id } } }";

pub(super) struct Remote {
    pub(super) item_id: String,
    pub(super) number: Option<u64>,
    pub(super) title: String,
    pub(super) body: String,
    pub(super) url: Option<String>,
    pub(super) option: Option<String>,
}

pub(super) async fn fetch_items(link: &Link) -> Result<Vec<Remote>, String> {
    let mut out = Vec::new();
    let mut after = Value::Null;
    for _ in 0..=(MAX_BOARD_CARDS / 100) {
        let data = graphql(ITEMS, json!({ "p": link.project_id, "after": after })).await?;
        let items = &data["node"]["items"];
        for node in items["nodes"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if node["isArchived"].as_bool() == Some(true) {
                continue;
            }
            let content = &node["content"];
            let kind = content["__typename"].as_str().unwrap_or_default();
            let (number, url) = match kind {
                "Issue" => {
                    if !content["repository"]["nameWithOwner"]
                        .as_str()
                        .is_some_and(|r| r.eq_ignore_ascii_case(&link.repo))
                    {
                        continue;
                    }
                    (content["number"].as_u64(), Some(text(&content["url"])))
                }
                "DraftIssue" => (None, None),
                _ => continue,
            };
            out.push(Remote {
                item_id: text(&node["id"]),
                number,
                title: text(&content["title"]),
                body: text(&content["body"]),
                url,
                option: node["fieldValueByName"]["optionId"]
                    .as_str()
                    .map(str::to_owned),
            });
        }
        if items["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
            break;
        }
        after = items["pageInfo"]["endCursor"].clone();
    }
    Ok(out)
}

pub(super) async fn set_status(link: &Link, item: &str, lane: BoardLane) -> Result<(), String> {
    let option = link
        .option_for(lane)
        .ok_or_else(|| format!("the project has no Status option for {}", lane.label()))?;
    graphql(
        SET_STATUS,
        json!({ "p": link.project_id, "i": item, "f": link.status_field_id, "o": option }),
    )
    .await
    .map(|_| ())
}

pub(super) async fn ensure_status_field(
    project_id: &str,
) -> Result<(String, BTreeMap<String, String>), String> {
    let data = graphql(STATUS_FIELD, json!({ "p": project_id })).await?;
    let field = &data["node"]["field"];
    let field_id = text(&field["id"]);
    if field_id.is_empty() {
        return Err("the project has no single-select Status field".to_owned());
    }
    let existing: Vec<Value> = field["options"].as_array().cloned().unwrap_or_default();
    let mut mapping = BTreeMap::new();
    let mut used = HashSet::new();
    for lane in BoardLane::ALL {
        if let Some(option) = existing.iter().find(|o| {
            let id = text(&o["id"]);
            !used.contains(&id) && aliases(lane).contains(&normalize(&text(&o["name"])).as_str())
        }) {
            let id = text(&option["id"]);
            used.insert(id.clone());
            mapping.insert(lane.as_str().to_owned(), id);
        }
    }
    if mapping.len() == BoardLane::ALL.len() {
        return Ok((field_id, mapping));
    }
    let mut list = Vec::new();
    for lane in BoardLane::ALL {
        match mapping
            .get(lane.as_str())
            .and_then(|id| existing.iter().find(|o| text(&o["id"]) == *id))
        {
            Some(option) => list.push(json!({
                "id": option["id"], "name": option["name"], "color": option["color"],
                "description": option["description"].as_str().unwrap_or_default(),
            })),
            None => list.push(
                json!({ "name": lane.label(), "color": lane_color(lane), "description": "" }),
            ),
        }
    }
    for option in existing.iter().filter(|o| !used.contains(&text(&o["id"]))) {
        list.push(json!({
            "id": option["id"], "name": option["name"], "color": option["color"],
            "description": option["description"].as_str().unwrap_or_default(),
        }));
    }
    let data = graphql(UPDATE_FIELD, json!({ "f": field_id, "o": list })).await?;
    let options = data["updateProjectV2Field"]["projectV2Field"]["options"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut mapping = BTreeMap::new();
    for lane in BoardLane::ALL {
        if let Some(option) = options
            .iter()
            .find(|o| aliases(lane).contains(&normalize(&text(&o["name"])).as_str()))
        {
            mapping.insert(lane.as_str().to_owned(), text(&option["id"]));
        }
    }
    if mapping.len() != BoardLane::ALL.len() {
        return Err("could not add the Harness lanes to the project's Status field".to_owned());
    }
    Ok((field_id, mapping))
}

pub(super) fn split_repo(repo: &str) -> Result<(&str, &str), String> {
    repo.split_once('/')
        .filter(|(o, n)| !o.is_empty() && !n.is_empty())
        .ok_or_else(|| format!("bad repository name {repo}"))
}
