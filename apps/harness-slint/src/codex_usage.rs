//! Codex plan quota from the local Codex login.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::plan_usage::{resets_in, Fetched};

mod app_server;
mod reset;

use app_server::AppServer;
pub(crate) use reset::{consume_reset, verify_reset};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

pub(crate) enum ResetOutcome {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
    UsageStillBlocked,
    RecoveryPending(String),
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
        .or_else(|| limits.get("primaryWindow"))
        .or_else(|| limits.get("primary"));
    let secondary = limits
        .get("secondary_window")
        .or_else(|| limits.get("secondaryWindow"))
        .or_else(|| limits.get("secondary"));

    let mut five = None;
    let mut week = None;
    let mut without_duration = [None, None];
    for (position, window) in [primary, secondary].into_iter().enumerate() {
        let Some(window) = window else {
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
            .or_else(|| window.get("limitWindowSeconds"))
            .and_then(Value::as_i64)
            .or_else(|| {
                window
                    .get("windowDurationMins")
                    .or_else(|| window.get("window_duration_mins"))
                    .and_then(Value::as_i64)
                    .map(|minutes| minutes.saturating_mul(60))
            });
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
        let slot = match duration {
            Some(18_000) => &mut five,
            Some(604_800) => &mut week,
            Some(_) => continue,
            None => &mut without_duration[position],
        };
        slot.get_or_insert((used_val, reset_label));
    }

    // A window without a duration may fill an empty slot but cannot replace an explicit match.
    match (five.is_some(), week.is_some()) {
        (false, false) => [five, week] = without_duration,
        (false, true) => five = without_duration.into_iter().flatten().next(),
        (true, false) => week = without_duration.into_iter().flatten().next(),
        (true, true) => {}
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
    let reset_eligible = account_id.is_some();
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

#[cfg(test)]
mod tests {
    use super::row;
    use serde_json::json;

    #[test]
    fn duration_metadata_wins_over_window_position() {
        for (primary, secondary, expected) in [
            (Some(300), Some(10_080), (Some(20.0), Some(80.0))),
            (Some(10_080), Some(300), (Some(80.0), Some(20.0))),
            (Some(10_080), None, (Some(80.0), Some(20.0))),
            (None, Some(300), (Some(80.0), Some(20.0))),
            (None, None, (Some(20.0), Some(80.0))),
            (Some(60), Some(300), (Some(80.0), None)),
            (Some(60), None, (None, Some(80.0))),
            (Some(300), Some(300), (Some(20.0), None)),
        ] {
            let result = row(
                &json!({
                    "rateLimits": {
                        "primary": { "usedPercent": 20, "windowDurationMins": primary },
                        "secondary": { "usedPercent": 80, "windowDurationMins": secondary }
                    }
                }),
                None,
            );
            assert_eq!(
                (
                    result.five.map(|(used, _)| used),
                    result.week.map(|(used, _)| used)
                ),
                expected,
                "primary duration {primary:?}, secondary duration {secondary:?}",
            );
        }
    }

    #[test]
    fn reset_request_requires_account_but_no_local_usage_threshold() {
        let data = json!({
            "rateLimits": { "primary": { "usedPercent": 5 } },
            "rateLimitResetCredits": { "availableCount": 1 }
        });
        assert!(!row(&data, None).reset_eligible);
        assert!(row(&data, Some("test-account".into())).reset_eligible);
    }
}
