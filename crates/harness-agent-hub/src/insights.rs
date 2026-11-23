//! Phone Usage / Pulls tabs: token use summed from the transcript store, open pull requests of the
//! known projects through `gh pr list` (read-only, nothing is written).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use harness_core_api::chat::{ContextUsage, TokenUsage};
use harness_core_api::{
    ApiResult, ListProjects, PullError, PullInfo, PullList, UsageCost, UsageRow, UsageStats,
};
use rusqlite::{params, Connection};
use serde_json::Value;

use crate::error::HubError;
use crate::hub::Hub;
use crate::store_chat::parse_json;
use crate::util::now_ms;

const DAY_MS: i64 = 86_400_000;
/// Projects asked for pull requests, newest first (each is one `gh` call).
const MAX_PULL_PROJECTS: usize = 12;
const PR_FIELDS: &str =
    "number,title,url,state,isDraft,author,headRefName,updatedAt,reviewDecision,additions,deletions";

type Tokens = BTreeMap<(String, i64), UsageRow>;

fn usage_rows(c: &Connection, since: i64) -> Result<(Tokens, HashMap<String, f64>), HubError> {
    let mut statement = c.prepare(
        "SELECT session_id, COALESCE(agent_id, ''), created_at, usage_json, context_usage_json
         FROM turns
         WHERE session_id IN (SELECT DISTINCT session_id FROM turns WHERE created_at >= ?1)
           AND (usage_json IS NOT NULL OR context_usage_json IS NOT NULL)
         ORDER BY session_id, seq",
    )?;
    let rows = statement
        .query_map(params![since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .filter_map(Result::ok);
    let mut tokens = Tokens::new();
    let mut costs: HashMap<String, f64> = HashMap::new();
    // 成本是每个 (会话, agent) 的累计值：只把落在范围内的增量记上。
    let mut last: HashMap<(String, String), f64> = HashMap::new();
    for (session, agent, created, usage, context) in rows {
        let inside = created >= since;
        if let Some(usage) = parse_json::<TokenUsage>(usage).filter(|_| inside) {
            let day = created - created.rem_euclid(DAY_MS);
            let row = tokens
                .entry((agent.clone(), day))
                .or_insert_with(|| UsageRow {
                    agent_id: agent.clone(),
                    day,
                    turns: 0,
                    input: 0,
                    output: 0,
                    cached_read: 0,
                    cached_write: 0,
                    thought: 0,
                });
            row.turns += 1;
            row.input += usage.input;
            row.output += usage.output;
            row.cached_read += usage.cached_read.unwrap_or(0);
            row.cached_write += usage.cached_write.unwrap_or(0);
            row.thought += usage.thought.unwrap_or(0);
        }
        let Some(amount) = parse_json::<ContextUsage>(context)
            .and_then(|c| c.cost)
            .filter(|cost| cost.currency.eq_ignore_ascii_case("USD"))
            .and_then(|cost| cost.amount.trim().parse::<f64>().ok())
            .filter(|a| a.is_finite() && *a >= 0.0)
        else {
            continue;
        };
        let previous = last.insert((session, agent.clone()), amount).unwrap_or(0.0);
        if inside {
            let delta = if amount >= previous {
                amount - previous
            } else {
                amount
            };
            *costs.entry(agent).or_default() += delta;
        }
    }
    Ok((tokens, costs))
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn parse_pulls(project: &str, path: &str, raw: &str) -> Vec<PullInfo> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| PullInfo {
            project: project.to_owned(),
            project_path: path.to_owned(),
            number: number(item, "number"),
            title: text(item, "title"),
            url: text(item, "url"),
            state: text(item, "state"),
            is_draft: item
                .get("isDraft")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            author: item
                .get("author")
                .map(|a| text(a, "login"))
                .unwrap_or_default(),
            branch: text(item, "headRefName"),
            updated_at: text(item, "updatedAt"),
            review_decision: text(item, "reviewDecision"),
            additions: number(item, "additions"),
            deletions: number(item, "deletions"),
        })
        .collect()
}

/// Errors that only mean "this folder has no GitHub repository" are not worth showing.
fn quiet(error: &str) -> bool {
    let lower = error.to_lowercase();
    [
        "not a git repository",
        "no git remotes",
        "none of the git remotes",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn project_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

async fn project_pulls(gh: PathBuf, path: String) -> Result<Vec<PullInfo>, (String, String)> {
    let name = project_name(&path);
    let args = [
        "pr", "list", "--state", "open", "--limit", "30", "--json", PR_FIELDS,
    ];
    let out = crate::gh::run(&gh, &args, Some(Path::new(&path)), None)
        .await
        .and_then(|out| out.into_result("gh pr list"))
        .map_err(|e| (name.clone(), e))?;
    Ok(parse_pulls(&name, &path, &out))
}

impl Hub {
    pub(crate) async fn do_usage_stats(&self, days: u32) -> ApiResult<UsageStats> {
        self.check_open()?;
        let days = days.clamp(1, 366);
        let now = now_ms();
        let since = if days == 1 {
            now - DAY_MS
        } else {
            now - now.rem_euclid(DAY_MS) - DAY_MS * (i64::from(days) - 1)
        };
        let (tokens, costs) = self.ctx.store.run(move |c| usage_rows(c, since)).await?;
        let mut costs: Vec<UsageCost> = costs
            .into_iter()
            .filter(|(_, amount)| *amount > 0.0)
            .map(|(agent_id, amount)| UsageCost {
                agent_id,
                amount,
                currency: "USD".into(),
            })
            .collect();
        costs.sort_by(|a, b| b.amount.total_cmp(&a.amount));
        Ok(UsageStats {
            days,
            since,
            rows: tokens.into_values().collect(),
            costs,
        })
    }

    pub(crate) async fn do_list_pulls(&self) -> ApiResult<PullList> {
        let Some((gh, _)) = crate::gh::resolve_gh() else {
            return Ok(PullList {
                gh_missing: true,
                ..PullList::default()
            });
        };
        let projects = self.do_list_projects(ListProjects::default()).await?;
        let mut jobs = tokio::task::JoinSet::new();
        for project in projects
            .into_iter()
            .filter(|p| Path::new(&p.workspace_path).is_dir())
            .take(MAX_PULL_PROJECTS)
        {
            jobs.spawn(project_pulls(gh.clone(), project.workspace_path));
        }
        let mut list = PullList::default();
        while let Some(done) = jobs.join_next().await {
            match done {
                Ok(Ok(prs)) => list.prs.extend(prs),
                Ok(Err((project, error))) if !quiet(&error) => {
                    list.errors.push(PullError { project, error });
                }
                _ => {}
            }
        }
        list.prs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        list.errors.sort_by(|a, b| a.project.cmp(&b.project));
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulls_parse_gh_json() {
        let raw = r#"[{"number":7,"title":"Fix","url":"u","state":"OPEN","isDraft":true,
            "author":{"login":"octo"},"headRefName":"fix/x","updatedAt":"2026-11-20T10:00:00Z",
            "reviewDecision":"","additions":3,"deletions":1}]"#;
        let prs = parse_pulls("Worse", "D:/Worse", raw);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].author, "octo");
        assert!(prs[0].is_draft);
        assert_eq!(prs[0].branch, "fix/x");
        assert_eq!(parse_pulls("a", "b", "oops"), Vec::new());
        assert!(quiet("fatal: not a git repository (or any parent)"));
    }
}
