//! Read-only summaries for the phone's Usage and Pulls tabs: token use per agent and day from the
//! transcript store, and open pull requests of the known projects (via `gh`).

use serde::{Deserialize, Serialize};

/// Token use of one agent on one UTC day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRow {
    pub agent_id: String,
    /// Unix ms of the UTC day start.
    pub day: i64,
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cached_read: u64,
    pub cached_write: u64,
    pub thought: u64,
}

/// USD spent by one agent in the range (from cumulative ACP `usage_update` costs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    pub agent_id: String,
    pub amount: f64,
    pub currency: String,
}

/// `usage_stats` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub days: u32,
    /// Unix ms the range starts at.
    pub since: i64,
    pub rows: Vec<UsageRow>,
    pub costs: Vec<UsageCost>,
}

/// One pull request of a known project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullInfo {
    pub project: String,
    pub project_path: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub is_draft: bool,
    pub author: String,
    pub branch: String,
    pub updated_at: String,
    pub review_decision: String,
    pub additions: u64,
    pub deletions: u64,
}

/// A project whose pull requests could not be listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullError {
    pub project: String,
    pub error: String,
}

/// `list_pulls` result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullList {
    /// `gh` is not installed (Settings → Setup installs it).
    pub gh_missing: bool,
    pub prs: Vec<PullInfo>,
    pub errors: Vec<PullError>,
}
