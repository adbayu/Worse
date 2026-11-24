//! "Redeem saved Codex reset" on the Usage page: one attempt at a time, retried with the same
//! idempotency key after an ambiguous result.

use crate::codex_usage::{ResetError, ResetOutcome};

use super::{publish, refresh_with, CodexResetAttempt, STATE};

pub(crate) fn redeem_codex_reset() {
    let started = STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let s = cell.as_mut()?;
        let account_id = s.codex_account_id.clone()?;
        let retry = s.codex_reset_attempts.contains_key(&account_id);
        if s.codex_reset_busy
            || s.busy
            || s.wanted
            || (!retry
                && !s.all.iter().any(|row| {
                    row.id.as_str() == "codex" && row.reset_count > 0 && row.reset_eligible
                }))
        {
            return None;
        }
        s.codex_reset_busy = true;
        s.codex_reset_error = false;
        let attempt = s
            .codex_reset_attempts
            .entry(account_id.clone())
            .or_insert_with(|| CodexResetAttempt {
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                credit_id: None,
                recovery_pending: false,
            })
            .clone();
        s.codex_reset_message = if attempt.recovery_pending {
            "Checking Codex reset status…".into()
        } else {
            "Redeeming saved Codex reset…".into()
        };
        Some((attempt, account_id, retry))
    });
    let Some((attempt, expected_account_id, retry)) = started else {
        return;
    };
    let attempt_account_id = expected_account_id.clone();
    let retry_credit_id = attempt.credit_id.clone();
    publish();
    std::thread::spawn(move || {
        let result = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) if attempt.recovery_pending => {
                Ok(runtime.block_on(crate::codex_usage::verify_reset(&expected_account_id)))
            }
            Ok(runtime) => runtime.block_on(crate::codex_usage::consume_reset(
                attempt.idempotency_key,
                expected_account_id,
                retry,
                retry_credit_id.clone(),
            )),
            Err(error) => Err(ResetError {
                message: format!("Codex reset runtime: {error}"),
                preserve_key: retry,
                credit_id: retry_credit_id.clone(),
            }),
        };
        let recovery_pending = matches!(&result, Ok(ResetOutcome::RecoveryPending(_)));
        let clear_attempt = match &result {
            Ok(_) => !recovery_pending,
            Err(error) => !error.preserve_key,
        };
        let retry_credit_id = result
            .as_ref()
            .err()
            .filter(|error| error.preserve_key)
            .and_then(|error| error.credit_id.clone());
        let clear_quota = matches!(
            &result,
            Ok(ResetOutcome::Reset | ResetOutcome::AlreadyRedeemed)
        );
        let _ = slint::invoke_from_event_loop(move || {
            let (message, error) = reset_message(result);
            STATE.with(|cell| {
                if let Some(s) = cell.borrow_mut().as_mut() {
                    s.codex_reset_busy = false;
                    s.codex_reset_message = message;
                    s.codex_reset_error = error;
                    if clear_attempt {
                        s.codex_reset_attempts.remove(&attempt_account_id);
                    } else if let Some(attempt) =
                        s.codex_reset_attempts.get_mut(&attempt_account_id)
                    {
                        attempt.recovery_pending |= recovery_pending;
                        if let Some(credit_id) = retry_credit_id {
                            attempt.credit_id = Some(credit_id);
                        }
                    }
                }
            });
            if clear_quota {
                crate::bridge::send(crate::core::Command::ClearAgentQuota("codex".into()));
            }
            publish();
            refresh_with(true);
        });
    });
}

fn reset_message(result: Result<ResetOutcome, ResetError>) -> (String, bool) {
    match result {
        Ok(ResetOutcome::Reset) => {
            ("Reset applied; refreshing Codex usage.".to_owned(), false)
        }
        Ok(ResetOutcome::NothingToReset) => {
            ("No covered Codex limit needs a reset yet.".to_owned(), true)
        }
        Ok(ResetOutcome::NoCredit) => {
            ("No saved Codex reset is available.".to_owned(), true)
        }
        Ok(ResetOutcome::UsageStillBlocked) => (
            "Codex consumed the reset, but still reports usage blocked. Local quota stays paused.".to_owned(),
            true,
        ),
        Ok(ResetOutcome::RecoveryPending(reason)) => (
            format!("Reset applied, but usage could not be verified. {reason}. Check the reset status again."),
            true,
        ),
        Ok(ResetOutcome::AlreadyRedeemed) => (
            "Reset already applied; refreshing Codex usage.".to_owned(),
            false,
        ),
        Err(error) => (error.message, true),
    }
}
