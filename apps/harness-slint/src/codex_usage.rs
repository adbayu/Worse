//! Codex plan quota from the local Codex login.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::plan_usage::{resets_in, Fetched};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

#[derive(Clone, Copy)]
pub(crate) enum ResetOutcome {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
}

pub(crate) async fn fetch() -> Fetched {
    match tokio::time::timeout(Duration::from_secs(25), fetch_usage()).await {
        Ok(Ok(value)) => row(&value),
        Ok(Err(error)) => failed(&error),
        Err(_) => failed("Codex usage request timed out"),
    }
}

async fn fetch_usage() -> Result<Value, String> {
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
    response
        .json::<Value>()
        .await
        .map_err(|_| "Codex returned invalid usage data".into())
}

async fn fetch_app_server() -> Result<Value, String> {
    let response = app_server_request(json!({
        "jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read", "params": {}
    }))
    .await?;
    if response.get("error").is_some() {
        return Err("Codex app-server usage unavailable".into());
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| "Codex returned no usage data".into())
}

pub(crate) async fn consume_reset(idempotency_key: String) -> Result<ResetOutcome, String> {
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app_server_request(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "account/rateLimitResetCredit/consume",
            "params": {"idempotencyKey": idempotency_key}
        })),
    )
    .await
    .map_err(|_| "Codex reset request timed out".to_owned())??;
    if response.get("error").is_some() {
        return Err(response
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("Codex reset request failed")
            .to_owned());
    }
    match response.pointer("/result/outcome").and_then(Value::as_str) {
        Some("reset") => Ok(ResetOutcome::Reset),
        Some("nothingToReset") => Ok(ResetOutcome::NothingToReset),
        Some("noCredit") => Ok(ResetOutcome::NoCredit),
        Some("alreadyRedeemed") => Ok(ResetOutcome::AlreadyRedeemed),
        _ => Err("Codex returned an unknown reset result".into()),
    }
}

async fn app_server_request(request: Value) -> Result<Value, String> {
    let program = harness_agent_hub::agents::resolve_program("codex")
        .unwrap_or_else(|| PathBuf::from("codex"));
    let mut command = Command::new(program);
    command
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot start Codex: {error}"))?;
    let mut input = child.stdin.take().ok_or("Codex stdin unavailable")?;
    let output = child.stdout.take().ok_or("Codex stdout unavailable")?;
    let mut output = BufReader::new(output).lines();

    send(
        &mut input,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "worse-harness",
                    "title": "Worse Harness",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {}
            }
        }),
    )
    .await?;
    let initialized = response(&mut output, 1).await?;
    if initialized.get("error").is_some() {
        return Err("Codex app-server initialization failed".into());
    }
    send(
        &mut input,
        json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
    )
    .await?;
    send(&mut input, request).await?;
    response(&mut output, 2).await
}

async fn send(input: &mut tokio::process::ChildStdin, value: Value) -> Result<(), String> {
    input
        .write_all(value.to_string().as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    input
        .write_all(b"\n")
        .await
        .map_err(|error| error.to_string())
}

async fn response(
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<Value, String> {
    while let Some(line) = output
        .next_line()
        .await
        .map_err(|error| error.to_string())?
    {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message["id"].as_u64() == Some(id) {
            return Ok(message);
        }
    }
    Err("Codex app-server closed before returning usage".into())
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

fn row(value: &Value) -> Fetched {
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
            .and_then(Value::as_i64)
            .or_else(|| {
                let seconds = window
                    .get("reset_after_seconds")
                    .or_else(|| window.get("resetAfterSeconds"))
                    .and_then(Value::as_i64)?;
                let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
                Some(i64::try_from(now).ok()?.saturating_add(seconds))
            });
        *target = Some((
            used.clamp(0.0, 100.0) as f32,
            reset.map(resets_in).unwrap_or_default(),
        ));
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
    let reset_eligible = five.as_ref().is_some_and(|(used, _)| *used > 20.0)
        || week.as_ref().is_some_and(|(used, _)| *used > 20.0);
    let mut summary = Vec::new();
    if let Some((used, reset)) = &five {
        summary.push(if reset.is_empty() {
            format!("5-hour {:.0}% used", used)
        } else {
            format!("5-hour {used:.0}% used · {reset}")
        });
    }
    if let Some((used, reset)) = &week {
        summary.push(if reset.is_empty() {
            format!("Weekly {:.0}% used", used)
        } else {
            format!("Weekly {used:.0}% used · {reset}")
        });
    }
    let unavailable = five.is_none() && week.is_none();
    Fetched {
        id: "codex".into(),
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
