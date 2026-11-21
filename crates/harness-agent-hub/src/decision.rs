//! The decision model: a small, cheap model that makes Harness's routing choices.
//!
//! Harness makes a few choices by a fixed rule. The first one handed to a model is delegation: when an agent
//! calls `delegate` without naming an agent, the rule was "the first available subagent". With a decision
//! model set, Harness asks it which agent of the team (and which of that agent's models) suits the task, and
//! falls back to the rule when the model is off, slow, unreachable or answers nonsense.
//!
//! The decision model is one of the user's API providers (`providers.json`): any OpenAI-compatible Chat
//! Completions endpoint, for example `OpenRouter` or Command Code's Provider API with a small model. One
//! request per decision, no tools, a JSON answer.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// `<home>/decision.json`.
pub const FILE: &str = "decision.json";
/// A decision that takes longer is dropped and the rule decides.
const TIMEOUT: Duration = Duration::from_secs(20);
const TASK_CHARS: usize = 3_000;
/// Models listed per agent in the question (an agent can advertise dozens).
const MODELS_PER_AGENT: usize = 40;

/// The user's choice. Read each time, so a change in Settings applies at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Id of the API provider that answers (`api-…` in `providers.json`); empty = no decision model.
    pub provider: String,
    /// Model to ask; empty = the provider's own model.
    pub model: String,
    /// Let it pick the agent (and model) of a `delegate` call that names none.
    pub delegation: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
            delegation: true,
        }
    }
}

impl Config {
    /// A provider is set.
    #[must_use]
    pub fn active(&self) -> bool {
        !self.provider.trim().is_empty()
    }
}

/// The stored choice; the default when the file is missing or unreadable.
#[must_use]
pub fn load(home: &Path) -> Config {
    std::fs::read_to_string(home.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Stores the choice.
///
/// # Errors
///
/// I/O errors.
pub fn save(home: &Path, config: &Config) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let text = serde_json::to_string_pretty(config).unwrap_or_else(|_| "{}".into());
    std::fs::write(home.join(FILE), text)
}

/// One agent the task could go to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    /// Orchestra role ("Subagent", "QC / Reviewer", …); empty outside an Orchestra team.
    pub role: String,
    /// First line of the Orchestra instructions of that agent.
    pub note: String,
    /// `(id, name)` of the models the agent offers; empty when it has one fixed model.
    pub models: Vec<(String, String)>,
}

/// What the decision model chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub agent: String,
    /// One of the chosen agent's models, when it offers several and the answer named one of them.
    pub model: Option<String>,
    pub reason: String,
}

const SYSTEM: &str = "You route one task to one of several coding agents. Pick the agent that suits the \
task best. Weigh each agent's role and notes. Prefer a cheaper or faster agent for simple, mechanical work \
and a stronger one for design, hard debugging and review. If the agent you pick lists models, also pick one \
of its models by the same reasoning; otherwise leave model empty. Answer with one JSON object and nothing \
else: {\"agent\": \"<agent id>\", \"model\": \"<model id or empty>\", \"reason\": \"<one short sentence>\"}";

/// The Chat Completions request that asks for a choice.
#[must_use]
pub fn request_body(model: &str, task: &str, candidates: &[Candidate]) -> Value {
    use std::fmt::Write as _;
    let task: String = task.chars().take(TASK_CHARS).collect();
    let mut question = format!("Task:\n{task}\n\nAgents:\n");
    for c in candidates {
        let _ = write!(question, "- id: {}; name: {}", c.id, c.name);
        if !c.role.is_empty() {
            let _ = write!(question, "; role: {}", c.role);
        }
        if !c.note.is_empty() {
            let _ = write!(question, "; notes: {}", c.note);
        }
        if !c.models.is_empty() {
            let list: Vec<&str> = c
                .models
                .iter()
                .take(MODELS_PER_AGENT)
                .map(|(id, _)| id.as_str())
                .collect();
            let _ = write!(question, "; models: {}", list.join(", "));
        }
        question.push('\n');
    }
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": question}
        ],
        "temperature": 0,
        "max_tokens": 300,
        "stream": false
    })
}

/// Reads the model's answer: the JSON object in it, checked against the candidates. An agent that is not a
/// candidate is no answer; a model the agent does not offer is dropped.
#[must_use]
pub fn parse(content: &str, candidates: &[Candidate]) -> Option<Choice> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    let answer: Value = serde_json::from_str(content.get(start..=end)?).ok()?;
    let wanted = answer.get("agent")?.as_str()?.trim();
    let candidate = candidates.iter().find(|c| c.id == wanted).or_else(|| {
        candidates
            .iter()
            .find(|c| c.id.eq_ignore_ascii_case(wanted) || c.name.eq_ignore_ascii_case(wanted))
    })?;
    let model = answer
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .and_then(|m| {
            candidate
                .models
                .iter()
                .find(|(id, name)| id == m || name.eq_ignore_ascii_case(m))
                .map(|(id, _)| id.clone())
        });
    let reason: String = answer
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect();
    Some(Choice {
        agent: candidate.id.clone(),
        model,
        reason,
    })
}

/// Asks the decision model which candidate should do `task`.
///
/// # Errors
///
/// Why there is no decision (off, provider gone, request failed, unusable answer); the caller then decides by
/// its rule.
pub async fn choose(home: &Path, task: &str, candidates: &[Candidate]) -> Result<Choice, String> {
    let config = load(home);
    if !config.active() || !config.delegation {
        return Err("no decision model is set".into());
    }
    if candidates.len() < 2 && candidates.iter().all(|c| c.models.len() < 2) {
        return Err("nothing to choose between".into());
    }
    let providers = crate::providers::load(home)
        .await
        .map_err(|e| e.to_string())?;
    let entry = providers
        .providers
        .iter()
        .find(|p| p.id == config.provider && !p.is_command_code())
        .ok_or_else(|| {
            format!(
                "the decision provider {} is not configured",
                config.provider
            )
        })?;
    let model = if config.model.trim().is_empty() {
        entry.model.clone()
    } else {
        config.model.trim().to_owned()
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(TIMEOUT)
        .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut request = client
        .post(format!(
            "{}/chat/completions",
            entry.base_url.trim_end_matches('/')
        ))
        .json(&request_body(&model, task, candidates));
    if !entry.api_key.trim().is_empty() {
        request = request.bearer_auth(entry.api_key.trim());
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("decision request failed: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("the decision model answered {status}"));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|e| format!("decision answer is not JSON: {e}"))?;
    let content = body
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    parse(content, candidates)
        .ok_or_else(|| "the decision model did not name one of the agents".to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
