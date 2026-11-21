use super::{ApiResult, BoxFuture, Deserialize, FileHit, Serialize, SessionSummary};

// ---- command center -------------------------------------------------------------------------------

/// What the command center searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchScope {
    Actions,
    Sessions,
    Files,
}

/// One UI action offered to the ranker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionEntry {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// `command_search` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSearch {
    pub query: String,
    pub workspace_path: Option<String>,
    /// Empty = all scopes.
    pub scopes: Vec<SearchScope>,
    pub actions: Vec<ActionEntry>,
    /// Per scope; 0 → 10, max 50.
    pub limit: u32,
}

/// A ranked action id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredId {
    pub id: String,
    pub score: u32,
}

/// A ranked session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHit {
    pub summary: SessionSummary,
    /// Transcript excerpt around the match (empty for a title match).
    pub snippet: String,
    pub score: u32,
}

/// `command_search` result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSearchResult {
    pub actions: Vec<ScoredId>,
    pub sessions: Vec<SessionHit>,
    pub files: Vec<FileHit>,
    /// False when no file source is injected.
    pub files_available: bool,
}

/// File-name search the hub borrows from the UI's workspace index (single owner of the index).
pub trait FileSearchSource: Send + Sync {
    /// Files and folders of `workspace_path` matching `query`.
    fn search(
        &self,
        workspace_path: String,
        query: String,
        limit: u32,
    ) -> BoxFuture<'_, ApiResult<Vec<FileHit>>>;
}
