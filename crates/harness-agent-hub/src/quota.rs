//! Error classification (spec §6.3) and the quota registry (single owner of exhaustion state).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use harness_core_api::{AgentId, ErrorClass, QuotaState};
use serde_json::Value;

use crate::agents::ErrorDialect;
use crate::util::now_secs;

#[cfg(test)]
mod limit_text_tests;
mod reset_text;

pub(crate) use reset_text::*;

pub(crate) const CODEX_USAGE_LIMIT_WINDOW_SECS: i64 = 5 * 60 * 60;

/// What we know about one agent failure.
#[derive(Debug, Clone, Default)]
pub struct AgentErrorInfo {
    /// JSON-RPC error code, when the failure was a JSON-RPC error.
    pub code: Option<i64>,
    pub message: String,
    /// JSON-RPC error `data`.
    pub data: Option<Value>,
}

/// Classifies a failure. Order: explicit Harness convention, Codex `codexErrorInfo`, dialect texts,
/// generic texts.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn classify_generic(info: &AgentErrorInfo, dialect: ErrorDialect) -> ErrorClass {
    let data = info.data.as_ref();
    let resets_at = data
        .and_then(find_resets_at)
        .or_else(|| parse_reset_from_text(&info.message));
    let fallback_5h =
        || (dialect == ErrorDialect::Codex).then(|| now_secs() + CODEX_USAGE_LIMIT_WINDOW_SECS);
    if let Some(kind) = data
        .and_then(|d| d.get("harnessErrorKind"))
        .and_then(Value::as_str)
    {
        match kind {
            "quota_exhausted" => {
                return ErrorClass::QuotaExhausted {
                    resets_at: resets_at.or_else(fallback_5h),
                }
            }
            "transient" => {
                return ErrorClass::Transient {
                    retry_after_secs: data
                        .and_then(|d| d.get("retryAfterSecs"))
                        .and_then(Value::as_u64),
                }
            }
            _ => {}
        }
    }
    // Codex app-server `TurnError.codexErrorInfo` (verified from `codex app-server generate-json-schema`,
    // codex-cli 0.166.0). How codex-acp forwards it is unverified, so we look for it anywhere in `data`.
    if let Some(info_kind) = data.and_then(|d| find_key(d, "codexErrorInfo")) {
        if let Some(kind) = info_kind.as_str() {
            match kind {
                "usageLimitExceeded" | "sessionBudgetExceeded" => {
                    return ErrorClass::QuotaExhausted {
                        resets_at: resets_at.or_else(fallback_5h),
                    }
                }
                "rateLimitExceeded" | "serverOverloaded" | "internalServerError" => {
                    return ErrorClass::Transient {
                        retry_after_secs: None,
                    }
                }
                _ => {}
            }
        } else if info_kind.get("httpConnectionFailed").is_some()
            || info_kind.get("responseStreamConnectionFailed").is_some()
        {
            return ErrorClass::Transient {
                retry_after_secs: None,
            };
        }
    }
    let text = format!(
        "{} {}",
        info.message,
        data.map(Value::to_string).unwrap_or_default()
    )
    .to_ascii_lowercase();
    let quota_markers: &[&str] = match dialect {
        // Claude Code texts (Claude Agent SDK `error: "rate_limit" | "billing_error"`, CLI
        // "Claude AI usage limit reached|<unix>", "... limit reached ∙ resets ..."): unverified locally.
        ErrorDialect::ClaudeCode => &[
            "usage limit",
            "limit reached",
            "hit your",
            "-hour limit",
            "weekly limit",
            "session limit",
            "billing_error",
            "credit balance",
            "out of credits",
        ],
        ErrorDialect::Codex => &[
            "usage limit",
            "usagelimitexceeded",
            "hit your usage limit",
            "session limit",
            "quota",
        ],
        ErrorDialect::Agy | ErrorDialect::Generic => &[
            "usage limit",
            "limit reached",
            "hit your",
            "-hour limit",
            "weekly limit",
            "quota",
            "out of credits",
            "credit balance",
            "resource_exhausted",
        ],
    };
    if quota_markers.iter().any(|m| text.contains(m)) {
        return ErrorClass::QuotaExhausted {
            resets_at: resets_at.or_else(fallback_5h),
        };
    }
    let transient_markers = [
        "rate limit",
        "rate_limit",
        "overloaded",
        "timed out",
        "timeout",
        "econnreset",
        "econnrefused",
        "network",
        "503",
        "529",
    ];
    if transient_markers.iter().any(|m| text.contains(m)) {
        return ErrorClass::Transient {
            retry_after_secs: None,
        };
    }
    ErrorClass::Fatal
}

fn find_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|v| find_key(v, key))),
        Value::Array(items) => items.iter().find_map(|v| find_key(v, key)),
        _ => None,
    }
}

fn find_resets_at(data: &Value) -> Option<i64> {
    for key in [
        "resetsAt",
        "resets_at",
        "resetAt",
        "reset_at",
        "reset_after_seconds",
        "resetAfterSeconds",
        "retry_after",
        "retryAfter",
    ] {
        let Some(value) = find_key(data, key) else {
            continue;
        };
        if matches!(
            key,
            "reset_after_seconds" | "resetAfterSeconds" | "retry_after" | "retryAfter"
        ) {
            let text = value.as_str().map(str::trim);
            if let Some(secs) = value.as_i64().or_else(|| text?.parse::<i64>().ok()) {
                if secs >= 0 {
                    return Some(now_secs().saturating_add(secs));
                }
            } else if let Some(at) = text.and_then(parse_iso8601_utc) {
                return Some(at);
            }
        } else if let Some(at) = value
            .as_i64()
            .map(normalize_epoch)
            .or_else(|| value.as_str().and_then(parse_epoch_or_iso))
        {
            return Some(at);
        }
    }
    None
}

/// Accepts seconds or milliseconds.
pub(super) fn normalize_epoch(n: i64) -> i64 {
    if n > 100_000_000_000 {
        n / 1000
    } else {
        n
    }
}
/// Single owner of per-agent exhaustion state (persisted by the hub through the store).
#[derive(Debug, Clone, Default)]
pub struct QuotaRegistry {
    inner: Arc<Mutex<HashMap<AgentId, QuotaState>>>,
}

impl QuotaRegistry {
    /// Registry seeded from persisted rows.
    #[must_use]
    pub fn with_state(state: HashMap<AgentId, QuotaState>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(state)),
        }
    }

    /// Current state; an exhaustion whose reset time has passed reads as `Ok`.
    #[must_use]
    pub fn get(&self, agent: &AgentId) -> QuotaState {
        let Ok(map) = self.inner.lock() else {
            return QuotaState::Ok;
        };
        match map.get(agent) {
            Some(QuotaState::Exhausted {
                resets_at: Some(t), ..
            }) if *t <= now_secs() => QuotaState::Ok,
            Some(state) => state.clone(),
            None => QuotaState::Ok,
        }
    }

    /// Clears a quota only when it is still the state whose reset timer fired.
    #[must_use]
    pub fn clear_expired_if_unchanged(&self, agent: &AgentId, expected: &QuotaState) -> bool {
        let QuotaState::Exhausted {
            resets_at: Some(reset),
            ..
        } = expected
        else {
            return false;
        };
        if *reset > now_secs() {
            return false;
        }
        let Ok(mut map) = self.inner.lock() else {
            return false;
        };
        if map.get(agent) != Some(expected) {
            return false;
        }
        map.insert(agent.clone(), QuotaState::Ok);
        true
    }

    /// True when the agent may be selected by failover.
    #[must_use]
    pub fn is_usable(&self, agent: &AgentId) -> bool {
        matches!(self.get(agent), QuotaState::Ok)
    }

    /// Sets the state.
    pub fn set(&self, agent: &AgentId, state: QuotaState) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(agent.clone(), state);
        }
    }

    /// Earliest future reset among exhausted agents.
    #[must_use]
    pub fn earliest_reset(&self) -> Option<i64> {
        let now = now_secs();
        let map = self.inner.lock().ok()?;
        map.values()
            .filter_map(|s| match s {
                QuotaState::Exhausted {
                    resets_at: Some(t), ..
                } if *t > now => Some(*t),
                _ => None,
            })
            .min()
    }
}

/// Formats a unix time as `YYYY-MM-DDTHH:MM:SSZ` for display.
#[must_use]
pub fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}
