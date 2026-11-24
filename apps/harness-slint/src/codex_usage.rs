//! Codex plan quota from the local Codex login.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::plan_usage::{resets_in, Fetched};

mod app_server;
mod reset;

use app_server::AppServer;
pub(crate) use reset::consume_reset;

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

#[derive(Clone, Copy)]
pub(crate) enum ResetOutcome {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
    UsageStillBlocked,
    UsageStatusUnavailable,
}

pub(crate) struct ResetError {
    pub message: String,
    pub preserve_key: bool,
    pub credit_id: Option<String>,
}

impl ResetError {
    fn after_consume(message: impl Into<String>, credit_id: String) -> Self {
        Self {
            message: message.into(),
            preserve_key: true,
            credit_id: Some(credit_id),
        }
    }

    fn rejected(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            preserve_key: false,
            credit_id: None,
        }
    }

    fn retry_preflight(message: impl Into<String>, retry: bool, credit_id: Option<String>) -> Self {
        Self {
            message: message.into(),
            preserve_key: retry,
            credit_id,
        }
    }
}

pub(crate) async fn fetch() -> Fetched {
    match tokio::time::timeout(Duration::from_secs(25), fetch_usage()).await {
        Ok(Ok((value, account_id))) => row(&value, account_id),
        Ok(Err(error)) => failed(&error),
        Err(_) => failed("Codex usage request timed out"),
    }
}

async fn fetch_usage() -> Result<(Value, Option<String>), String> {
    let Ok(path) = auth_path() else {
        return fetch_app_server().await;
    };
    let Ok(bytes) = tokio::fs::read(path).await else {
        return fetch_app_server().await;
    };
    let Ok(auth) = serde_json::from_slice::<Value>(&bytes) else {
        return fetch_app_server().await;
    };
    drop(bytes);
    let tokens = &auth["tokens"];
    let Some(token) = tokens["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
    else {
        return fetch_app_server().await;
    };
    let account_id = tokens["account_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned);

    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(25))
        .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("Codex usage client: {error}"))?;
    let mut request = client
        .get(USAGE_URL)
        .bearer_auth(token)
        .header("accept", "application/json")
        .header("origin", "https://chatgpt.com")
        .header("referer", "https://chatgpt.com/");
    if let Some(account_id) = tokens["account_id"].as_str().filter(|id| !id.is_empty()) {
        request = request.header("ChatGPT-Account-Id", account_id);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Codex usage network error")?;
    let status = response.status();
    if status.as_u16() == 401 {
        return fetch_app_server().await;
    }
    if !status.is_success() {
        return Err(format!(
            "Codex usage unavailable (HTTP {})",
            status.as_u16()
        ));
    }
    let value = response
        .json::<Value>()
        .await
        .map_err(|_| "Codex returned invalid usage data")?;
    Ok((value, account_id))
}

async fn fetch_app_server() -> Result<(Value, Option<String>), String> {
    let mut app_server = AppServer::start().await?;
    let response = app_server
        .request(
            2,
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read", "params": {}
            }),
        )
        .await?;
    if response.get("error").is_some() {
        return Err("Codex app-server usage unavailable".into());
    }
    let value = response
        .get("result")
        .cloned()
        .ok_or_else(|| "Codex returned no usage data".to_owned())?;
    let account_id = value
        .get("accountId")
        .or_else(|| value.get("account_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    Ok((value, account_id))
}

pub(crate) fn has_eligible_limit(rates: &Value) -> bool {
    let snapshot_is_eligible = |snapshot: &Value| {
        [
            "primary",
            "primaryWindow",
            "primary_window",
            "secondary",
            "secondaryWindow",
            "secondary_window",
        ]
        .into_iter()
        .any(|name| {
            snapshot
                .get(name)
                .and_then(|window| {
                    window
                        .get("usedPercent")
                        .or_else(|| window.get("used_percent"))
                })
                .and_then(Value::as_f64)
                .is_some_and(|used| used > 20.0)
        })
    };
    if rates
        .get("rateLimits")
        .or_else(|| rates.get("rate_limits"))
        .or_else(|| rates.get("rate_limit"))
        .is_some_and(snapshot_is_eligible)
    {
        return true;
    }
    rates
        .get("rateLimitsByLimitId")
        .or_else(|| rates.get("rate_limits_by_limit_id"))
        .and_then(Value::as_object)
        .is_some_and(|limits| limits.values().any(snapshot_is_eligible))
        || rates
            .get("additionalRateLimits")
            .or_else(|| rates.get("additional_rate_limits"))
            .and_then(Value::as_array)
            .is_some_and(|limits| {
                limits.iter().any(|limit| {
                    limit
                        .get("rateLimit")
                        .or_else(|| limit.get("rate_limit"))
                        .is_some_and(snapshot_is_eligible)
                })
            })
}

fn auth_path() -> Result<PathBuf, String> {
    if let Some(home) = std::env::var_os("CODEX_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(home).join("auth.json"));
    }
    std::env::var_os("USERPROFILE")
        .filter(|home| !home.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|home| !home.is_empty()))
        .map(|home| PathBuf::from(home).join(".codex").join("auth.json"))
        .ok_or_else(|| "Cannot find Codex login".into())
}

#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
fn row(value: &Value, account_id: Option<String>) -> Fetched {
    let limits = value
        .get("rate_limit")
        .or_else(|| {
            value
                .get("rateLimitsByLimitId")
                .and_then(|limits| limits.get("codex"))
        })
        .or_else(|| value.get("rateLimits"))
        .unwrap_or(value);
    let primary = limits
        .get("primary_window")
        .or_else(|| limits.get("primary"));
    let secondary = limits
        .get("secondary_window")
        .or_else(|| limits.get("secondary"));
    let mut five = None;
    let mut week = None;
    for (position, window) in [("primary", primary), ("secondary", secondary)] {
        let Some(window) = window.filter(|value| value.is_object()) else {
            continue;
        };
        let Some(used) = window
            .get("used_percent")
            .or_else(|| window.get("usedPercent"))
            .and_then(Value::as_f64)
        else {
            continue;
        };
        let duration = window
            .get("limit_window_seconds")
            .and_then(Value::as_i64)
            .or_else(|| {
                window
                    .get("windowDurationMins")
                    .and_then(Value::as_i64)
                    .map(|minutes| minutes.saturating_mul(60))
            });
        // Duration identifies windows even when Codex returns them in reverse order.
        let target = match duration {
            Some(18_000) => &mut five,
            Some(604_800) => &mut week,
            Some(_) => continue,
            None if position == "primary" => &mut five,
            None => &mut week,
        };
        let reset = window
            .get("reset_at")
            .or_else(|| window.get("resetsAt"))
            .and_then(|v| {
                v.as_i64()
                    .map(|n| if n > 100_000_000_000 { n / 1000 } else { n })
                    .or_else(|| v.as_str().and_then(crate::plan_usage::parse_rfc3339))
            })
            .or_else(|| {
                let seconds = window
                    .get("reset_after_seconds")
                    .or_else(|| window.get("resetAfterSeconds"))
                    .and_then(Value::as_i64)?;
                let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
                Some(i64::try_from(now).ok()?.saturating_add(seconds))
            });
        let used_val = used.clamp(0.0, 100.0) as f32;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .unwrap_or(0);
        let reset_label = if used_val <= 0.0 || reset.is_some_and(|at| at <= now) {
            String::new()
        } else {
            reset.map(resets_in).unwrap_or_default()
        };
        *target = Some((used_val, reset_label));
    }
    let plan = value
        .get("plan_type")
        .or_else(|| value.get("planType"))
        .or_else(|| limits.get("planType"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reset_count = value
        .get("rate_limit_reset_credits")
        .or_else(|| value.get("rateLimitResetCredits"))
        .and_then(|credits| {
            credits
                .get("available_count")
                .or_else(|| credits.get("availableCount"))
        })
        .and_then(Value::as_i64)
        .filter(|count| *count >= 0)
        .and_then(|count| i32::try_from(count).ok());
    let reset_eligible = account_id.is_some() && has_eligible_limit(value);
    let mut summary = Vec::new();
    if let Some((used, reset)) = &five {
        summary.push(if reset.is_empty() {
            format!("5-hour {used:.0}% used")
        } else {
            format!("5-hour {used:.0}% used · {reset}")
        });
    }
    if let Some((used, reset)) = &week {
        summary.push(if reset.is_empty() {
            format!("Weekly {used:.0}% used")
        } else {
            format!("Weekly {used:.0}% used · {reset}")
        });
    }
    let unavailable = five.is_none() && week.is_none();
    Fetched {
        id: "codex".into(),
        account_id,
        label: "Codex".into(),
        plan: plan.into(),
        email: String::new(),
        error: if unavailable {
            "Usage unavailable".into()
        } else {
            String::new()
        },
        five,
        week,
        opus: None,
        reset_count,
        reset_eligible,
        labels: Default::default(),
        summary: summary.join(" · "),
        bars: Vec::new(),
    }
}

fn failed(error: &str) -> Fetched {
    Fetched {
        id: "codex".into(),
        account_id: None,
        label: "Codex".into(),
        plan: String::new(),
        email: String::new(),
        error: error.into(),
        five: None,
        week: None,
        opus: None,
        reset_count: None,
        reset_eligible: false,
        labels: Default::default(),
        summary: String::new(),
        bars: Vec::new(),
    }
}
