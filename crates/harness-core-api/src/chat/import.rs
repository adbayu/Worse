use super::{Deserialize, Serialize, SessionSummary};

// ---- import ---------------------------------------------------------------------------------------

/// History source (decision 14A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportSource {
    /// `ZCode` (legacy JSON snapshots and the CLI store).
    Zcode,
    ClaudeCode,
    Codex,
}

impl ImportSource {
    /// Prefix of `imported_from`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zcode => "zcode",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }
}

/// `import_scan` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportScan {
    /// Empty = all sources.
    pub sources: Vec<ImportSource>,
}

/// A session that can be imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    pub source: ImportSource,
    pub external_id: String,
    pub title: String,
    pub workspace_path: String,
    pub turn_count: u32,
    pub updated_at: i64,
    /// Already imported (dedupe).
    pub imported: bool,
}

/// `import_sessions` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    pub source: ImportSource,
    pub external_ids: Vec<String>,
}

/// `import_sessions` result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: Vec<SessionSummary>,
    /// `(external_id, reason)`.
    pub skipped: Vec<(String, String)>,
}
