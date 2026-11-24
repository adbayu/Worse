use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use harness_core_api as api;
use serde_json::Value;
use slint::ComponentHandle;

use crate::ui::{AppWindow, ChatUi, PlanLimit};

mod row;
use row::{now_ms, sv, title_case, to_row, window_of};
pub(crate) use row::{parse_rfc3339, resets_in};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const MIN_INTERVAL: Duration = Duration::from_secs(30);

struct Account {
    id: String,
    label: String,
    dir: PathBuf,
}

#[derive(Clone)]
struct CodexResetAttempt {
    idempotency_key: String,
    credit_id: Option<String>,
}

struct State {
    window: slint::Weak<AppWindow>,
    accounts: Vec<Account>,
    agy: Vec<crate::agy_usage::AgySlot>,
    last: Option<Instant>,
    busy: bool,
    wanted: bool,
    codex_reset_busy: bool,
    codex_reset_attempts: std::collections::HashMap<String, CodexResetAttempt>,
    codex_account_id: Option<String>,
    codex_reset_message: String,
    codex_reset_error: bool,
    all: Vec<PlanLimit>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub fn install(window: &AppWindow) {
    STATE.with(|cell| {
        *cell.borrow_mut() = Some(State {
            window: window.as_weak(),
            accounts: Vec::new(),
            agy: Vec::new(),
            last: None,
            busy: false,
            wanted: false,
            codex_reset_busy: false,
            codex_reset_attempts: std::collections::HashMap::new(),
            codex_account_id: None,
            codex_reset_message: String::new(),
            codex_reset_error: false,
            all: Vec::new(),
        });
    });
}

pub fn refresh() {
    refresh_with(false);
}

fn refresh_with(force: bool) {
    let due = STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(s) = cell.as_mut() else {
            return false;
        };
        if s.codex_reset_busy
            || s.busy
            || s.wanted
            || (!force && s.last.is_some_and(|t| t.elapsed() < MIN_INTERVAL))
        {
            return false;
        }
        s.wanted = true;
        true
    });
    publish();
    if due {
        // agy 账号列表先到，随后的 Claude 账号列表触发读取。
        crate::bridge::send(crate::core::Command::AgyAccounts(api::AgyAccountOp::List));
        crate::bridge::send(crate::core::Command::Accounts(
            crate::core::AccountsCall::List,
        ));
    }
}

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
        s.codex_reset_message = "Redeeming saved Codex reset…".into();
        let attempt = s
            .codex_reset_attempts
            .entry(account_id.clone())
            .or_insert_with(|| CodexResetAttempt {
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                credit_id: None,
            })
            .clone();
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
            Ok(runtime) => runtime.block_on(crate::codex_usage::consume_reset(
                attempt.idempotency_key,
                expected_account_id,
                retry,
                retry_credit_id.clone(),
            )),
            Err(error) => Err(crate::codex_usage::ResetError {
                message: format!("Codex reset runtime: {error}"),
                preserve_key: retry,
                credit_id: retry_credit_id.clone(),
            }),
        };
        let clear_attempt = match &result {
            Ok(_) => true,
            Err(error) => !error.preserve_key,
        };
        let retry_credit_id = result
            .as_ref()
            .err()
            .filter(|error| error.preserve_key)
            .map(|error| error.credit_id.clone());
        let clear_quota = matches!(
            &result,
            Ok(crate::codex_usage::ResetOutcome::Reset)
                | Ok(crate::codex_usage::ResetOutcome::AlreadyRedeemed)
        );
        let _ = slint::invoke_from_event_loop(move || {
            let (message, error) = match result {
                Ok(crate::codex_usage::ResetOutcome::Reset) => {
                    ("Reset applied; refreshing Codex usage.".to_owned(), false)
                }
                Ok(crate::codex_usage::ResetOutcome::NothingToReset) => {
                    ("No covered Codex limit needs a reset yet.".to_owned(), true)
                }
                Ok(crate::codex_usage::ResetOutcome::NoCredit) => {
                    ("No saved Codex reset is available.".to_owned(), true)
                }
                Ok(crate::codex_usage::ResetOutcome::UsageStillBlocked) => (
                    "Codex consumed the reset, but still reports usage blocked. Local quota stays paused.".to_owned(),
                    true,
                ),
                Ok(crate::codex_usage::ResetOutcome::UsageStatusUnavailable) => (
                    "Codex consumed the reset, but recovery status is unavailable. Local quota stays paused.".to_owned(),
                    true,
                ),
                Ok(crate::codex_usage::ResetOutcome::AlreadyRedeemed) => (
                    "Reset already applied; refreshing Codex usage.".to_owned(),
                    false,
                ),
                Err(error) => (error.message, true),
            };
            STATE.with(|cell| {
                if let Some(s) = cell.borrow_mut().as_mut() {
                    s.codex_reset_busy = false;
                    s.codex_reset_message = message;
                    s.codex_reset_error = error;
                    if clear_attempt {
                        s.codex_reset_attempts.remove(&attempt_account_id);
                    } else if let Some(credit_id) = retry_credit_id {
                        if let Some(attempt) = s.codex_reset_attempts.get_mut(&attempt_account_id) {
                            attempt.credit_id = credit_id;
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

/// Saved agy accounts (from every `Event::AgyAccounts`); each gets its own usage row.
pub fn agy_accounts(result: &Result<api::AgyAccounts, String>) {
    let Ok(list) = result else {
        return;
    };
    STATE.with(|cell| {
        if let Some(s) = cell.borrow_mut().as_mut() {
            s.agy = list
                .accounts
                .iter()
                .filter(|a| a.signed_in)
                .map(|a| crate::agy_usage::AgySlot {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    active: a.active,
                })
                .collect();
        }
    });
}

pub fn accounts(result: &Result<Vec<api::AccountInfo>, String>) {
    let job = STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let s = cell.as_mut()?;
        if let Ok(rows) = result {
            s.accounts = rows
                .iter()
                .map(|a| Account {
                    id: a.id.as_str().to_owned(),
                    label: a.label.clone(),
                    dir: PathBuf::from(&a.profile_dir),
                })
                .collect();
        }
        if !s.wanted || s.busy {
            return None;
        }
        s.wanted = false;
        s.busy = true;
        let mut list = Vec::new();
        if let Some(dir) = default_dir() {
            list.push(Account {
                id: "claude-code".into(),
                label: "Claude Code".into(),
                dir,
            });
        }
        list.extend(s.accounts.iter().map(|a| Account {
            id: a.id.clone(),
            label: a.label.clone(),
            dir: a.dir.clone(),
        }));
        Some((s.window.clone(), list, s.agy.clone()))
    });
    if let Some((window, list, agy)) = job {
        fetch(window, list, agy);
    }
}

pub(crate) fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("USERPROFILE")
        .filter(|d| !d.is_empty())
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".claude"))
}

pub(crate) struct Fetched {
    pub id: String,
    pub account_id: Option<String>,
    pub label: String,
    pub plan: String,
    pub email: String,
    pub error: String,
    pub five: Option<(f32, String)>,
    pub week: Option<(f32, String)>,
    pub opus: Option<(f32, String)>,
    pub reset_count: Option<i32>,
    pub reset_eligible: bool,
    pub labels: [String; 3],
    pub summary: String,
    pub bars: Vec<(String, f32, String)>,
}

fn fetch(window: slint::Weak<AppWindow>, list: Vec<Account>, agy: Vec<crate::agy_usage::AgySlot>) {
    std::thread::spawn(move || {
        let rows = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(fetch_all(list, agy)),
            Err(error) => list
                .into_iter()
                .map(|a| failed(a, &format!("runtime: {error}")))
                .collect(),
        };
        crate::remote::publish_limits(rows.iter().map(remote_limit).collect());
        let codex_account_id = rows
            .iter()
            .find(|row| row.id == "codex")
            .and_then(|row| row.account_id.clone());
        let _ = slint::invoke_from_event_loop(move || {
            STATE.with(|cell| {
                if let Some(s) = cell.borrow_mut().as_mut() {
                    s.busy = false;
                    s.last = Some(Instant::now());
                }
            });
            let model: Vec<PlanLimit> = rows.into_iter().map(to_row).collect();
            STATE.with(|cell| {
                if let Some(s) = cell.borrow_mut().as_mut() {
                    s.codex_account_id = codex_account_id;
                    s.all = model;
                }
            });
            if window.upgrade().is_some() {
                publish();
            }
        });
    });
}

/// One account for the phone's Usage › Limits tab (percent used, 0–100).
fn remote_limit(f: &Fetched) -> serde_json::Value {
    let used = |slot: &Option<(f32, String)>| slot.as_ref().map(|(pct, _)| f64::from(*pct));
    let reset = |slot: &Option<(f32, String)>| slot.as_ref().map(|(_, r)| r.clone());
    serde_json::json!({
        "id": f.id, "label": f.label, "plan": f.plan, "error": f.error,
        "known": f.five.is_some() || f.week.is_some(),
        "sessionUsed": used(&f.five), "sessionReset": reset(&f.five),
        "weekUsed": used(&f.week), "weekReset": reset(&f.week),
        "opusUsed": used(&f.opus),
        "sessionLabel": f.labels[0], "weekLabel": f.labels[1], "opusLabel": f.labels[2],
        "bars": f.bars.iter().map(|(label, used, reset)| serde_json::json!({
            "label": label, "used": f64::from(*used), "reset": reset,
        })).collect::<Vec<_>>(),
    })
}

fn publish() {
    let job = STATE.with(|cell| {
        let cell = cell.borrow();
        let s = cell.as_ref()?;
        let active: HashSet<String> = s
            .agy
            .iter()
            .filter(|a| a.active)
            .map(|a| a.id.clone())
            .collect();
        Some((
            s.window.upgrade()?,
            s.all.clone(),
            active,
            s.codex_reset_busy || s.busy || s.wanted,
            s.codex_account_id
                .as_ref()
                .is_some_and(|id| s.codex_reset_attempts.contains_key(id))
                && !s.codex_reset_busy,
            s.codex_reset_message.clone(),
            s.codex_reset_error,
        ))
    });
    if let Some((window, all, active, busy, retry, message, error)) = job {
        let chat = window.global::<ChatUi>();
        chat.set_codex_reset_busy(busy);
        chat.set_codex_reset_retry(retry);
        chat.set_codex_reset_message(sv(&message));
        chat.set_codex_reset_error(error);
        crate::plan_view::render(&window, &all, &active);
    }
}

fn failed(a: Account, error: &str) -> Fetched {
    Fetched {
        id: a.id,
        account_id: None,
        label: a.label,
        plan: String::new(),
        email: String::new(),
        error: error.to_owned(),
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

async fn fetch_all(list: Vec<Account>, agy: Vec<crate::agy_usage::AgySlot>) -> Vec<Fetched> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return list
                .into_iter()
                .map(|a| failed(a, &format!("http client: {error}")))
                .collect();
        }
    };
    let accounts_client = client.clone();
    let accounts = async move {
        let mut rows = Vec::with_capacity(list.len());
        for account in list {
            rows.push(fetch_one(&accounts_client, account).await);
        }
        rows
    };
    let (mut out, codex, agy) = tokio::join!(
        accounts,
        crate::codex_usage::fetch(),
        crate::agy_usage::fetch(&client, &agy),
    );
    out.push(codex);
    out.extend(agy);
    out
}

async fn fetch_one(client: &reqwest::Client, account: Account) -> Fetched {
    let email = account_email(&account).await;
    let mut row = fetch_usage(client, account).await;
    row.email = email;
    row
}

/// Email of the Claude account (`oauthAccount.emailAddress` in `.claude.json`): inside the profile folder, or
/// next to the default `~/.claude` folder.
async fn account_email(account: &Account) -> String {
    let mut files = vec![account.dir.join(".claude.json")];
    if account.id == "claude-code" {
        if let Some(parent) = account.dir.parent() {
            files.push(parent.join(".claude.json"));
        }
    }
    for file in files {
        let Ok(bytes) = tokio::fs::read(&file).await else {
            continue;
        };
        let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if let Some(email) = json["oauthAccount"]["emailAddress"]
            .as_str()
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            return email.to_owned();
        }
    }
    String::new()
}

async fn fetch_usage(client: &reqwest::Client, account: Account) -> Fetched {
    let path = account.dir.join(".credentials.json");
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return failed(account, "Not signed in");
        }
        Err(_) => return failed(account, "Cannot read login"),
    };
    let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
        return failed(account, "Cannot read login");
    };
    drop(bytes);
    let oauth = &json["claudeAiOauth"];
    let plan = oauth["subscriptionType"]
        .as_str()
        .map(title_case)
        .unwrap_or_default();
    let Some(token) = oauth["accessToken"].as_str().filter(|t| !t.is_empty()) else {
        return failed(account, "Not signed in");
    };
    let expired = oauth["expiresAt"].as_u64().is_some_and(|ms| ms <= now_ms());
    if expired {
        let mut row = failed(account, "Login expired; run Claude Code once to refresh it");
        row.plan = plan;
        return row;
    }
    let response = client
        .get(USAGE_URL)
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("accept", "application/json")
        .send()
        .await;
    let mut row = failed(account, "");
    row.plan = plan;
    let Ok(response) = response else {
        row.error = "Network error".into();
        return row;
    };
    let status = response.status();
    if !status.is_success() {
        row.error = if status.as_u16() == 401 {
            "Login expired; run Claude Code once to refresh it".into()
        } else {
            format!("Usage unavailable (HTTP {})", status.as_u16())
        };
        return row;
    }
    let Ok(body) = response.json::<Value>().await else {
        row.error = "Usage unavailable".into();
        return row;
    };
    row.five = window_of(&body["five_hour"]);
    row.week = window_of(&body["seven_day"]);
    row.opus = window_of(&body["seven_day_opus"]);
    if row.five.is_none() && row.week.is_none() {
        row.error = "Usage unavailable".into();
    }
    row
}
