use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use harness_core_api as api;
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::ui::{AppWindow, PlanBar, PlanLimit};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const MIN_INTERVAL: Duration = Duration::from_secs(30);

struct Account {
    id: String,
    label: String,
    dir: PathBuf,
}

struct State {
    window: slint::Weak<AppWindow>,
    accounts: Vec<Account>,
    agy: Vec<crate::agy_usage::AgySlot>,
    last: Option<Instant>,
    busy: bool,
    wanted: bool,
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
            all: Vec::new(),
        });
    });
}

pub fn refresh() {
    let due = STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(s) = cell.as_mut() else {
            return false;
        };
        if s.busy || s.last.is_some_and(|t| t.elapsed() < MIN_INTERVAL) {
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
    pub label: String,
    pub plan: String,
    pub email: String,
    pub error: String,
    pub five: Option<(f32, String)>,
    pub week: Option<(f32, String)>,
    pub opus: Option<(f32, String)>,
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
                    s.all = model;
                }
            });
            if window.upgrade().is_some() {
                publish();
            }
        });
    });
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
        Some((s.window.upgrade()?, s.all.clone(), active))
    });
    if let Some((window, all, active)) = job {
        crate::plan_view::render(&window, &all, &active);
    }
}

fn failed(a: Account, error: &str) -> Fetched {
    Fetched {
        id: a.id,
        label: a.label,
        plan: String::new(),
        email: String::new(),
        error: error.to_owned(),
        five: None,
        week: None,
        opus: None,
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
    let mut out = Vec::new();
    for account in list {
        out.push(fetch_one(&client, account).await);
    }
    out.extend(crate::agy_usage::fetch(&client, &agy).await);
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

fn window_of(v: &Value) -> Option<(f32, String)> {
    #[allow(clippy::cast_possible_truncation)]
    let used = v["utilization"].as_f64()? as f32;
    let reset = v["resets_at"]
        .as_str()
        .and_then(parse_rfc3339)
        .map(resets_in)
        .unwrap_or_default();
    Some((used.clamp(0.0, 100.0), reset))
}

fn to_row(f: Fetched) -> PlanLimit {
    let known = f.five.is_some() || f.week.is_some();
    let (five, five_reset) = f.five.unwrap_or_default();
    let (week, week_reset) = f.week.unwrap_or_default();
    let (opus, opus_reset) = f.opus.clone().unwrap_or_default();
    PlanLimit {
        id: sv(&f.id),
        label: sv(&f.label),
        plan: sv(&f.plan),
        email: sv(&f.email),
        error: sv(&f.error),
        known,
        five: five / 100.0,
        five_reset: sv(&five_reset),
        week: week / 100.0,
        week_reset: sv(&week_reset),
        has_opus: f.opus.is_some(),
        opus: opus / 100.0,
        opus_reset: sv(&opus_reset),
        five_label: sv(&f.labels[0]),
        week_label: sv(&f.labels[1]),
        opus_label: sv(&f.labels[2]),
        summary: sv(&f.summary),
        bars: ModelRc::new(VecModel::from(
            f.bars
                .iter()
                .map(|(label, used, reset)| PlanBar {
                    label: sv(label),
                    value: used / 100.0,
                    reset: sv(reset),
                })
                .collect::<Vec<_>>(),
        )),
    }
}

fn sv(text: &str) -> SharedString {
    SharedString::from(text)
}

fn title_case(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

pub(crate) fn resets_in(at: i64) -> String {
    let now = i64::try_from(now_ms() / 1000).unwrap_or(i64::MAX);
    let left = (at - now).max(0);
    let days = left / 86_400;
    let hours = (left % 86_400) / 3600;
    let minutes = (left % 3600) / 60;
    if days > 0 {
        format!("Resets in {days}d {hours}h")
    } else if hours > 0 {
        format!("Resets in {hours}h {minutes}m")
    } else {
        format!("Resets in {minutes}m")
    }
}

pub(crate) fn parse_rfc3339(text: &str) -> Option<i64> {
    let num = |s: &str| s.parse::<i64>().ok();
    let year = num(text.get(0..4)?)?;
    let month = num(text.get(5..7)?)?;
    let day = num(text.get(8..10)?)?;
    let hour = num(text.get(11..13)?)?;
    let minute = num(text.get(14..16)?)?;
    let second = num(text.get(17..19)?)?;
    let rest = text.get(19..)?;
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = if rest.is_empty() || rest.starts_with('Z') || rest.starts_with('z') {
        0
    } else {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let oh = num(rest.get(1..3)?)?;
        let om = num(rest.get(4..6)?)?;
        sign * (oh * 3600 + om * 60)
    };
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}
