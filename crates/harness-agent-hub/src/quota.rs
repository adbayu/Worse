//! Error classification (spec §6.3) and the quota registry (single owner of exhaustion state).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use harness_core_api::{AgentId, ErrorClass, QuotaState};
use serde_json::Value;

use crate::agents::ErrorDialect;
use crate::util::now_secs;

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
pub fn classify_generic(info: &AgentErrorInfo, dialect: ErrorDialect) -> ErrorClass {
    let data = info.data.as_ref();
    let mut resets_at = data
        .and_then(find_resets_at)
        .or_else(|| parse_reset_from_text(&info.message));
    let fallback_5h = || Some(now_secs() + 5 * 3600);
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
        if resets_at.is_none() && matches!(dialect, ErrorDialect::Codex | ErrorDialect::ClaudeCode)
        {
            resets_at = fallback_5h();
        }
        return ErrorClass::QuotaExhausted { resets_at };
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
    for key in ["resetsAt", "resets_at", "resetAt"] {
        if let Some(value) = find_key(data, key) {
            if let Some(n) = value.as_i64() {
                return Some(normalize_epoch(n));
            }
            if let Some(s) = value.as_str() {
                if let Some(n) = parse_epoch_or_iso(s) {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// Accepts seconds or milliseconds.
fn normalize_epoch(n: i64) -> i64 {
    if n > 100_000_000_000 {
        n / 1000
    } else {
        n
    }
}

/// `…|<unix>` suffix (Claude Code) or `resets at <ISO-8601>`, `try again at <clock>`, etc.
fn parse_reset_from_text(text: &str) -> Option<i64> {
    if let Some((_, tail)) = text.rsplit_once('|') {
        if let Ok(n) = tail.trim().parse::<i64>() {
            return Some(normalize_epoch(n));
        }
    }
    let lower = text.to_ascii_lowercase();
    for marker in ["resets at ", "resets in ", "try again at ", "resets "] {
        if let Some(index) = lower.find(marker) {
            let rest = &lower[index + marker.len()..];
            if let Some(token) = rest.split_whitespace().next() {
                let clean = token.trim_end_matches(&[')', '.', ',', ';'][..]);
                if let Some(n) = parse_epoch_or_iso(clean) {
                    return Some(n);
                }
            }
            if let Some(n) = parse_clock_from_rest(rest, chrono::Local::now()) {
                return Some(n);
            }
        }
    }
    None
}

fn parse_clock_from_rest(rest: &str, now: chrono::DateTime<chrono::Local>) -> Option<i64> {
    let mut words = rest.split_whitespace();
    let first = words.next()?.trim_end_matches(&[')', '.', ',', ';'][..]);
    if first.ends_with("am") || first.ends_with("pm") {
        return parse_clock(first, now);
    }
    if let Some(second) = words.next() {
        let second = second.trim_end_matches(&[')', '.', ',', ';'][..]);
        if second == "am" || second == "pm" {
            let combined = format!("{first}{second}");
            return parse_clock(&combined, now);
        }
    }
    parse_clock(first, now)
}

/// 修复：Claude Code 的 "resets 10:40am (Asia/Bangkok)" 与 Codex 的 "try again at 1:31 PM" 没有被解析，额度耗尽后没有恢复时间。
/// 按本机时区取下一次出现的该时刻。
fn parse_clock(token: &str, now: chrono::DateTime<chrono::Local>) -> Option<i64> {
    use chrono::{Duration, NaiveTime, TimeZone};
    let token = token
        .trim_end_matches(&[')', '.', ',', ';'][..])
        .to_ascii_lowercase();
    let (clock, pm) = if let Some(c) = token.strip_suffix("pm") {
        (c.trim(), true)
    } else {
        (token.strip_suffix("am")?.trim(), false)
    };
    let (hour, minute) = match clock.split_once(':') {
        Some((h, m)) => (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?),
        None => (clock.trim().parse::<u32>().ok()?, 0),
    };
    if !(1..=12).contains(&hour) {
        return None;
    }
    let hour = hour % 12 + if pm { 12 } else { 0 };
    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
    let mut at = chrono::Local
        .from_local_datetime(&now.date_naive().and_time(time))
        .earliest()?;
    if at <= now {
        at += Duration::days(1);
    }
    Some(at.timestamp())
}

fn parse_epoch_or_iso(s: &str) -> Option<i64> {
    if let Ok(n) = s.trim().parse::<i64>() {
        return Some(normalize_epoch(n));
    }
    parse_iso8601_utc(s.trim())
}

/// Minimal `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)` parser.
fn parse_iso8601_utc(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> { s.get(range)?.parse::<i64>().ok() };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = s.get(19..)?;
    if let Some(stripped) = rest.strip_prefix('.') {
        let digits = stripped.chars().take_while(char::is_ascii_digit).count();
        rest = stripped.get(digits..)?;
    }
    let offset = if rest.is_empty() || rest == "Z" || rest == "z" {
        0
    } else {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let h = rest.get(1..3)?.parse::<i64>().ok()?;
        let m = rest.get(4..6)?.parse::<i64>().ok()?;
        sign * (h * 3600 + m * 60)
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Howard Hinnant's days-from-civil.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
            let state = match state {
                QuotaState::Exhausted {
                    resets_at: None,
                    reason,
                } => QuotaState::Exhausted {
                    resets_at: Some(now_secs() + 5 * 3600),
                    reason,
                },
                other => other,
            };
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

#[cfg(test)]
mod limit_text_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use chrono::{Local, TimeZone, Timelike};

    #[test]
    fn claude_five_hour_limit_is_quota_with_reset() {
        let info = AgentErrorInfo {
            code: Some(-32603),
            message: "Internal error: You've hit your 5-hour limit · resets 10:40am (Asia/Bangkok)"
                .into(),
            data: None,
        };
        match classify_generic(&info, ErrorDialect::ClaudeCode) {
            ErrorClass::QuotaExhausted { resets_at } => {
                let at = Local.timestamp_opt(resets_at.unwrap(), 0).unwrap();
                assert_eq!((at.hour(), at.minute()), (10, 40));
            }
            other => panic!("expected quota, got {other:?}"),
        }
    }

    #[test]
    fn clock_reset_rolls_to_tomorrow_when_passed() {
        let now = Local.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        let at = parse_clock("10:40am", now).unwrap();
        assert_eq!(
            at,
            Local
                .with_ymd_and_hms(2026, 11, 16, 10, 40, 0)
                .unwrap()
                .timestamp()
        );
        let pm = parse_clock("3pm", now).unwrap();
        assert_eq!(
            pm,
            Local
                .with_ymd_and_hms(2026, 11, 15, 15, 0, 0)
                .unwrap()
                .timestamp()
        );
    }

    #[test]
    fn codex_try_again_at_parsed() {
        let info = AgentErrorInfo {
            code: None,
            message: "You’ve hit your usage limit. Upgrade to Pro, or try again at 1:31 PM.".into(),
            data: None,
        };
        match classify_generic(&info, ErrorDialect::Codex) {
            ErrorClass::QuotaExhausted {
                resets_at: Some(at),
            } => {
                let dt = Local.timestamp_opt(at, 0).unwrap();
                assert_eq!((dt.hour(), dt.minute()), (13, 31));
            }
            other => panic!("expected quota with reset, got {other:?}"),
        }
    }

    #[test]
    fn codex_exhaustion_without_time_falls_back_to_5h() {
        let info = AgentErrorInfo {
            code: None,
            message: "You've hit your usage limit.".into(),
            data: None,
        };
        let before = now_secs();
        match classify_generic(&info, ErrorDialect::Codex) {
            ErrorClass::QuotaExhausted {
                resets_at: Some(at),
            } => {
                assert!(at >= before + 5 * 3600 - 5);
                assert!(at <= before + 5 * 3600 + 5);
            }
            other => panic!("expected quota with 5h fallback, got {other:?}"),
        }
    }
}
