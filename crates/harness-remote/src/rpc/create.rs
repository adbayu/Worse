//! `create_session` for the phone: only inside a project the desktop already knows, never a worktree.

use harness_core_api::{AgentId, CreateSession, HarnessCore, ListProjects};
use serde::Deserialize;
use serde_json::Value;

use super::{out, strict};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewSession {
    #[serde(alias = "workspace_path")]
    workspace_path: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

/// Normalised path for comparing a requested workspace with the known projects.
pub(super) fn path_key(path: &str) -> String {
    path.trim()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

// 修复：手机端以前调用 create_session 被白名单拒绝；现在允许，但只能在桌面已有的项目里创建，
// 不接受任意路径或 worktree，避免远程设备把代理指向任意目录。
pub(super) async fn create_session(core: &dyn HarnessCore, params: Value) -> Result<Value, String> {
    let request: NewSession = strict(params)?;
    let wanted = path_key(&request.workspace_path);
    if wanted.is_empty() {
        return Err("invalid params: workspacePath is required".to_owned());
    }
    let projects = core
        .list_projects(ListProjects::default())
        .await
        .map_err(|error| error.to_string())?;
    let Some(project) = projects
        .into_iter()
        .find(|project| path_key(&project.workspace_path) == wanted)
    else {
        return Err("unknown project: open it on the desktop first".to_owned());
    };
    let agent = request
        .agent
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .map(|id| AgentId::from(id.as_str()));
    let title = request
        .title
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty());
    out(core
        .create_session(CreateSession {
            workspace_path: project.workspace_path,
            workspace_identity: None,
            title,
            agent,
            worktree: None,
        })
        .await)
}
