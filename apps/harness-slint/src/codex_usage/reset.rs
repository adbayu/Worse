//! Redeeming a saved Codex reset credit through `codex app-server`.

use serde_json::{json, Value};

use std::time::Duration;

use super::{AppServer, ResetError, ResetOutcome};

#[allow(clippy::too_many_lines)]
pub(crate) async fn consume_reset(
    idempotency_key: String,
    expected_account_id: String,
    retry: bool,
    retry_credit_id: Option<String>,
) -> Result<ResetOutcome, ResetError> {
    let mut app_server = tokio::time::timeout(Duration::from_secs(20), AppServer::start())
        .await
        .map_err(|_| {
            ResetError::retry_preflight(
                "Codex app-server start timed out",
                retry,
                retry_credit_id.clone(),
            )
        })?
        .map_err(|error| ResetError::retry_preflight(error, retry, retry_credit_id.clone()))?;
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
            retry_credit_id.clone(),
        )
    })?
    .map_err(|error| ResetError::retry_preflight(error, retry, retry_credit_id.clone()))?;
    if rates_response.get("error").is_some() {
        return Err(ResetError::retry_preflight(
            "Codex account verification failed; saved reset was not used",
            retry,
            retry_credit_id.clone(),
        ));
    }
    let rates = rates_response.get("result").ok_or_else(|| {
        ResetError::retry_preflight(
            "Codex account verification returned no usage data",
            retry,
            retry_credit_id.clone(),
        )
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
                retry_credit_id.clone(),
            )
        })?;
    if account_id != expected_account_id {
        return Err(ResetError::retry_preflight(
            "Codex login changed since usage was read; saved reset was not used",
            retry,
            retry_credit_id.clone(),
        ));
    }
    if retry_credit_id.is_none() {
        let available = rates
            .get("rateLimitResetCredits")
            .or_else(|| rates.get("rate_limit_reset_credits"))
            .and_then(|credits| {
                credits
                    .get("availableCount")
                    .or_else(|| credits.get("available_count"))
            })
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                ResetError::retry_preflight(
                    "Codex did not report saved reset count",
                    retry,
                    retry_credit_id.clone(),
                )
            })?;
        if available <= 0 {
            return Ok(ResetOutcome::NoCredit);
        }
    }
    let credit_id = retry_credit_id
        .or_else(|| available_reset_credit_id(rates))
        .ok_or_else(|| {
            ResetError::retry_preflight(
                "Codex did not provide a saved reset ID; redemption paused to protect the logged-in account",
                retry,
                None,
            )
        })?;
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app_server.request(
            3,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "account/rateLimitResetCredit/consume",
                "params": {"idempotencyKey": idempotency_key, "creditId": credit_id}
            }),
        ),
    )
    .await
    .map_err(|_| ResetError::after_consume("Codex reset request timed out", credit_id.clone()))?
    .map_err(|error| ResetError::after_consume(error, credit_id.clone()))?;
    if response.get("error").is_some() {
        return Err(ResetError::rejected(
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
                credit_id.clone(),
            ));
        }
    };
    Ok(check_usage_after_reset(&mut app_server, &expected_account_id, outcome).await)
}

/// Recheck an acknowledged reset without issuing another consumption request.
pub(crate) async fn verify_reset(expected_account_id: &str) -> ResetOutcome {
    let mut app_server = match tokio::time::timeout(Duration::from_secs(20), AppServer::start())
        .await
    {
        Ok(Ok(app_server)) => app_server,
        Ok(Err(error)) => return ResetOutcome::RecoveryPending(error),
        Err(_) => return ResetOutcome::RecoveryPending("Codex app-server start timed out".into()),
    };
    check_usage_after_reset(&mut app_server, expected_account_id, ResetOutcome::Reset).await
}

async fn check_usage_after_reset(
    app_server: &mut AppServer,
    expected_account_id: &str,
    outcome: ResetOutcome,
) -> ResetOutcome {
    match read_usage_allowed(app_server, expected_account_id).await {
        Ok(true) => outcome,
        Ok(false) => ResetOutcome::UsageStillBlocked,
        Err(error) => ResetOutcome::RecoveryPending(error),
    }
}

async fn read_usage_allowed(
    app_server: &mut AppServer,
    expected_account_id: &str,
) -> Result<bool, String> {
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
    .map_err(|_| "Codex usage status request timed out")??;
    if current_response.get("error").is_some() {
        return Err("Codex did not return usage status".into());
    }
    let current = current_response
        .get("result")
        .ok_or("Codex usage status was missing")?;
    let current_account_id = current
        .get("accountId")
        .or_else(|| current.get("account_id"))
        .and_then(Value::as_str);
    if current_account_id != Some(expected_account_id) {
        return Err("Codex login could not be verified".into());
    }
    current
        .get("ordinaryUsageAllowed")
        .or_else(|| current.get("ordinary_usage_allowed"))
        .and_then(Value::as_bool)
        .ok_or_else(|| "Codex did not report whether usage is allowed".into())
}

fn available_reset_credit_id(rates: &Value) -> Option<String> {
    let credits = rates
        .get("rateLimitResetCredits")
        .or_else(|| rates.get("rate_limit_reset_credits"))?
        .get("credits")?
        .as_array()?;
    credits
        .iter()
        .filter(|credit| {
            credit
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status.eq_ignore_ascii_case("available"))
        })
        .min_by_key(|credit| {
            credit
                .get("expiresAt")
                .or_else(|| credit.get("expires_at"))
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX)
        })
        .and_then(|credit| {
            credit
                .get("creditId")
                .or_else(|| credit.get("credit_id"))
                .or_else(|| credit.get("id"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}
