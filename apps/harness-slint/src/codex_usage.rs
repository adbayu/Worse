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
    UsageStillBlocked,
    UsageStatusUnavailable,
}

pub(crate) struct ResetError {
    pub message: String,
    pub preserve_key: bool,
}

impl ResetError {
    fn before_consume(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            preserve_key: false,
        }
    }

    fn after_consume(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            preserve_key: true,
        }
    }

    fn retry_preflight(message: impl Into<String>, retry: bool) -> Self {
        Self {
            message: message.into(),
            preserve_key: retry,
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

pub(crate) async fn consume_reset(
    idempotency_key: String,
    expected_account_id: String,
    retry: bool,
) -> Result<ResetOutcome, ResetError> {
    let mut app_server = tokio::time::timeout(Duration::from_secs(20), AppServer::start())
        .await
        .map_err(|_| ResetError::retry_preflight("Codex app-server start timed out", retry))?
        .map_err(|error| ResetError::retry_preflight(error, retry))?;
    let rates_response = tokio::time::timeout(
        Duration::from_secs(20),
        app_server.request(
            2,
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read", "params": {}
            }),
        ),
    )
    .await
    .map_err(|_| {
        ResetError::retry_preflight(
            "Codex account verification timed out; saved reset was not used",
            retry,
        )
    })?
    .map_err(|error| ResetError::retry_preflight(error, retry))?;
    if rates_response.get("error").is_some() {
        return Err(ResetError::retry_preflight(
            "Codex account verification failed; saved reset was not used",
            retry,
        ));
    }
    let rates = rates_response.get("result").ok_or_else(|| {
        ResetError::retry_preflight("Codex account verification returned no usage data", retry)
    })?;
    let account_id = rates
        .get("accountId")
        .or_else(|| rates.get("account_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            ResetError::retry_preflight(
                "Cannot verify Codex login; saved reset was not used",
                retry,
            )
        })?;
    if account_id != expected_account_id {
        return Err(ResetError::retry_preflight(
            "Codex login changed since usage was read; saved reset was not used",
            retry,
        ));
    }
    if !retry {
        let available = rates
            .get("rateLimitResetCredits")
            .or_else(|| rates.get("rate_limit_reset_credits"))
            .and_then(|credits| {
                credits
                    .get("availableCount")
                    .or_else(|| credits.get("available_count"))
            })
            .and_then(Value::as_i64)
            .ok_or_else(|| ResetError::before_consume("Codex did not report saved reset count"))?;
        if available <= 0 {
            return Ok(ResetOutcome::NoCredit);
        }
        if !has_eligible_limit(rates) {
            return Ok(ResetOutcome::NothingToReset);
        }
    }
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app_server.request(
            3,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "account/rateLimitResetCredit/consume",
                "params": {"idempotencyKey": idempotency_key}
            }),
        ),
    )
    .await
    .map_err(|_| ResetError::after_consume("Codex reset request timed out"))?
    .map_err(ResetError::after_consume)?;
    if response.get("error").is_some() {
        return Err(ResetError::after_consume(
            response
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Codex reset request failed"),
        ));
    }
    let outcome = match response.pointer("/result/outcome").and_then(Value::as_str) {
        Some("reset") => ResetOutcome::Reset,
        Some("nothingToReset") => return Ok(ResetOutcome::NothingToReset),
        Some("noCredit") => return Ok(ResetOutcome::NoCredit),
        Some("alreadyRedeemed") => ResetOutcome::AlreadyRedeemed,
        _ => {
            return Err(ResetError::after_consume(
                "Codex returned an unknown reset result",
            ));
        }
    };
    let current_response = tokio::time::timeout(
        Duration::from_secs(20),
        app_server.request(
            4,
            json!({
                "jsonrpc": "2.0", "id": 4, "method": "account/rateLimits/read", "params": {}
            }),
        ),
    )
    .await
    .map_err(|_| ResetError::after_consume("Codex reset applied, but recovery status timed out"))?
    .map_err(ResetError::after_consume)?;
    if current_response.get("error").is_some() {
        return Err(ResetError::after_consume(
            "Codex reset applied, but recovery status could not be read",
        ));
    }
    let current = current_response.get("result").ok_or_else(|| {
        ResetError::after_consume("Codex reset applied, but recovery status was missing")
    })?;
    let current_account_id = current
        .get("accountId")
        .or_else(|| current.get("account_id"))
        .and_then(Value::as_str);
    if current_account_id != Some(expected_account_id.as_str()) {
        return Err(ResetError::after_consume(
            "Codex reset applied, but the account changed before recovery could be verified",
        ));
    }
    match current
        .get("ordinaryUsageAllowed")
        .or_else(|| current.get("ordinary_usage_allowed"))
        .and_then(Value::as_bool)
    {
        Some(true) => Ok(outcome),
        Some(false) => Ok(ResetOutcome::UsageStillBlocked),
        None => Ok(ResetOutcome::UsageStatusUnavailable),
    }
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

struct AppServer {
    _child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

impl AppServer {
    async fn start() -> Result<Self, String> {
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
        let input = child.stdin.take().ok_or("Codex stdin unavailable")?;
        let output = child.stdout.take().ok_or("Codex stdout unavailable")?;
        let mut app_server = Self {
            _child: child,
            input,
            output: BufReader::new(output).lines(),
        };
        send(
            &mut app_server.input,
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
        let initialized = response(&mut app_server.output, 1).await?;
        if initialized.get("error").is_some() {
            return Err("Codex app-server initialization failed".into());
        }
        send(
            &mut app_server.input,
            json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
        )
        .await?;
        Ok(app_server)
    }

    async fn request(&mut self, id: u64, request: Value) -> Result<Value, String> {
        send(&mut self.input, request).await?;
        response(&mut self.output, id).await
    }
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
