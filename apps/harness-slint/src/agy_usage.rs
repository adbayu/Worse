//! Antigravity (agy) plan quota for the usage popover: reads agy's live login from the credential store
//! read-only, asks Cloud Code for the project and per-model quota, and groups models by family.
//! The access token only goes into the `Authorization` header; it is never logged or stored.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use harness_agent_hub::agy_cred::{slot_target, AGY_LIVE_TARGET};
use serde_json::{json, Value};

use crate::plan_usage::{parse_rfc3339, resets_in, Fetched};

const PROD: &str = "https://cloudcode-pa.googleapis.com/v1internal";
/// agy itself talks to the daily host; its quota summary is the one that moves with real use.
const DAILY: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal";
/// The agy CLI identity. Cloud Code only lists the Gemini 4 Argon tiers for the CLI flavour.
const CLI_VERSION: &str = "1.5.9";
const CLI_BUILD: &str = "1021519450";

pub(crate) const AGY_ID: &str = "agy";

/// One saved agy account (from the hub's account list).
#[derive(Clone, Debug)]
pub(crate) struct AgySlot {
    pub id: String,
    pub name: String,
    pub active: bool,
}

/// A bar before display: (label, used %, reset unix time).
type RawBar = (String, f32, Option<i64>);

/// Last successful quota per account id: (plan, bars, fetched at). Only numbers, no tokens.
type Snapshot = (String, Vec<RawBar>, i64);

static LAST: Mutex<Option<HashMap<String, Snapshot>>> = Mutex::new(None);

fn os_arch() -> (&'static str, &'static str) {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    (os, arch)
}

// 修复：旧的 IDE 风格 UA（antigravity/1.11.3 windows/amd64）拿不到 Gemini 4 Argon 分组，改用 agy CLI 的真实标识。
fn user_agent() -> String {
    let (os, arch) = os_arch();
    format!(
        "antigravity/cli/{CLI_VERSION} (aidev_client; os_type={os}; arch={arch}; cl={CLI_BUILD}; auth_method=consumer)"
    )
}

/// Cloud Code client platform enum for `loadCodeAssist` metadata.
fn platform_enum() -> u8 {
    match os_arch() {
        ("darwin", "amd64") => 1,
        ("darwin", "arm64") => 2,
        ("linux", "amd64") => 3,
        ("linux", "arm64") => 4,
        ("windows", "amd64") => 5,
        _ => 0,
    }
}

fn row(id: &str, label: &str, error: &str) -> Fetched {
    Fetched {
        id: id.into(),
        label: label.into(),
        plan: String::new(),
        error: error.to_owned(),
        five: None,
        week: None,
        opus: None,
        labels: Default::default(),
        summary: String::new(),
        bars: Vec::new(),
    }
}

fn read_token(target: &str) -> Result<Option<String>, String> {
    let store = crate::agy_cred::shared();
    if !store.0.available() {
        return Ok(None);
    }
    let Some(blob) = store.0.read(target)? else {
        return Err("Not signed in".into());
    };
    let json: Value = serde_json::from_slice(&blob.secret).map_err(|_| "Cannot read login")?;
    drop(blob);
    let token = &json["token"];
    let expired = token["expiry"]
        .as_str()
        .and_then(parse_rfc3339)
        .is_some_and(|at| at <= now_secs());
    if expired {
        return Err("Login expired; run agy once to refresh it".into());
    }
    token["access_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .map(|t| Some(t.to_owned()))
        .ok_or_else(|| "Not signed in".into())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

async fn post(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    method: &str,
    body: &Value,
) -> Result<Value, String> {
    let response = client
        .post(format!("{base}:{method}"))
        .bearer_auth(token)
        .header("user-agent", user_agent())
        .header("x-client-name", "antigravity")
        .header("x-client-version", CLI_VERSION)
        .json(body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| "Network error".to_owned())?;
    let status = response.status();
    if !status.is_success() {
        return Err(if status.as_u16() == 401 {
            "Login expired; run agy once to refresh it".into()
        } else {
            format!("Usage unavailable (HTTP {})", status.as_u16())
        });
    }
    response
        .json::<Value>()
        .await
        .map_err(|_| "Usage unavailable".to_owned())
}

/// One row per saved agy account (the active one from agy's live login, the others from their Harness copies);
/// without saved accounts one row for the live login. Empty when the credential store does not exist here.
pub(crate) async fn fetch(client: &reqwest::Client, slots: &[AgySlot]) -> Vec<Fetched> {
    if !crate::agy_cred::shared().0.available() {
        return Vec::new();
    }
    if slots.is_empty() {
        return vec![fetch_one(client, AGY_LIVE_TARGET, AGY_ID, "Antigravity").await];
    }
    let mut out = Vec::new();
    for slot in slots {
        let target = if slot.active {
            AGY_LIVE_TARGET.to_owned()
        } else {
            slot_target(&slot.id)
        };
        let id = format!("{AGY_ID}:{}", slot.id);
        let label = format!("Antigravity · {}", slot.name);
        out.push(fetch_one(client, &target, &id, &label).await);
    }
    out
}

async fn fetch_one(client: &reqwest::Client, target: &str, id: &str, label: &str) -> Fetched {
    match quota(client, target).await {
        Ok((plan, bars)) => {
            if let Ok(mut last) = LAST.lock() {
                last.get_or_insert_with(HashMap::new)
                    .insert(id.to_owned(), (plan.clone(), bars.clone(), now_secs()));
            }
            shown(id, label, plan, &bars, None)
        }
        // 修复：未使用的账号令牌约 1 小时后过期；显示该账号最后一次读到的额度，而不是只显示错误。
        Err(error) => {
            let cached = LAST
                .lock()
                .ok()
                .and_then(|last| last.as_ref()?.get(id).cloned());
            if let Some((plan, bars, at)) = cached {
                shown(id, label, plan, &bars, Some(at))
            } else {
                let mut out = row(id, label, &error);
                if error.starts_with("Login expired") && target != AGY_LIVE_TARGET {
                    out.error = "Login expired; switch to this account once to refresh it".into();
                }
                out
            }
        }
    }
}

/// Display row; `s_of` marks numbers remembered from an earlier check. A passed reset reads as unused.
fn shown(id: &str, label: &str, plan: String, bars: &[RawBar], as_of: Option<i64>) -> Fetched {
    let now = now_secs();
    let mut out = row(id, label, "");
    out.plan = plan;
    out.bars = bars
        .iter()
        .map(|(name, used, reset)| match reset {
            Some(at) if *at <= now => (name.clone(), 0.0, String::new()),
            Some(at) => (name.clone(), *used, resets_in(*at)),
            None => (name.clone(), *used, String::new()),
        })
        .collect();
    let mut top: Vec<&(String, f32, String)> = out.bars.iter().collect();
    top.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.summary = top
        .iter()
        .take(2)
        .map(|(name, used, _)| format!("{name} {used:.0}%"))
        .collect::<Vec<_>>()
        .join(" · ");
    if let Some(at) = as_of {
        let minutes = (now - at).max(0) / 60;
        let ago = if minutes < 60 {
            format!("{minutes}m")
        } else {
            format!("{}h", minutes / 60)
        };
        out.summary = format!("{} (as of {ago} ago)", out.summary);
    }
    if out.bars.is_empty() {
        out.error = "Usage unavailable".into();
    }
    out
}

/// Plan name and bars of the login in `target`.
async fn quota(client: &reqwest::Client, target: &str) -> Result<(String, Vec<RawBar>), String> {
    let token = read_token(target)?.ok_or_else(|| "Not signed in".to_owned())?;
    // 修复：metadata 改为 agy 实际发送的枚举数值（ideType 9 = ANTIGRAVITY，pluginType 2 = GEMINI）。
    let load = post(
        client,
        PROD,
        &token,
        "loadCodeAssist",
        &json!({"metadata": {"ideType": 9, "platform": platform_enum(), "pluginType": 2}}),
    )
    .await?;
    let plan = ["paidTier", "currentTier"]
        .iter()
        .find_map(|k| load[*k]["name"].as_str().filter(|n| !n.is_empty()))
        .unwrap_or_default()
        .to_owned();
    let project = match &load["cloudaicompanionProject"] {
        Value::String(s) => s.clone(),
        v => v["id"].as_str().unwrap_or_default().to_owned(),
    };
    let body = json!({"project": project});
    // 修复：额度先读 daily 主机（agy 的对话走这里，PROD 的数字一直停在 0%），失败再退回 PROD。
    let summary = post_either(client, &token, "retrieveUserQuotaSummary", &body).await;
    let mut bars = summary.as_ref().map(summary_bars).unwrap_or_default();
    let has = |bars: &[RawBar], word: &str| bars.iter().any(|(l, _, _)| l.contains(word));
    let no_gemini = !bars
        .iter()
        .any(|(l, _, _)| l.starts_with("Gemini") && !l.contains("Argon"));
    if no_gemini || !has(&bars, "Argon") {
        if let Some(models) = post_either(client, &token, "fetchAvailableModels", &body).await {
            let groups = group(&models["models"]);
            if no_gemini {
                if let Some((used, reset)) = groups[0] {
                    bars.push(("Gemini · 5-hour".into(), used, reset));
                }
            }
            if !has(&bars, "Argon") {
                if let Some((used, reset)) = groups[3] {
                    bars.push(("Gemini 4 Argon".into(), used, reset));
                }
            }
        }
    }
    Ok((plan, bars))
}

/// `method` on the daily host, then on PROD when daily fails.
async fn post_either(
    client: &reqwest::Client,
    token: &str,
    method: &str,
    body: &Value,
) -> Option<Value> {
    match post(client, DAILY, token, method, body).await {
        Ok(v) => Some(v),
        Err(_) => post(client, PROD, token, method, body).await.ok(),
    }
}
fn short_group(name: &str) -> String {
    let name = name
        .trim()
        .trim_end_matches(" models")
        .trim_end_matches(" Models");
    name.replace(" and ", " & ")
}

/// Every bucket of every quota group, as (label, used %, reset time), in server order.
fn summary_bars(summary: &Value) -> Vec<RawBar> {
    let mut out = Vec::new();
    for group in summary["groups"].as_array().into_iter().flatten() {
        let name = short_group(group["displayName"].as_str().unwrap_or("Models"));
        for bucket in group["buckets"].as_array().into_iter().flatten() {
            if bucket["disabled"].as_bool() == Some(true) {
                continue;
            }
            let Some(remaining) = bucket["remainingFraction"].as_f64() else {
                continue;
            };
            let window = match bucket["window"].as_str().unwrap_or_default() {
                "weekly" => "weekly".to_owned(),
                "5h" => "5-hour".to_owned(),
                "" => bucket["displayName"].as_str().unwrap_or("limit").to_owned(),
                other => other.to_owned(),
            };
            #[allow(clippy::cast_possible_truncation)]
            let used = ((1.0 - remaining.clamp(0.0, 1.0)) * 100.0) as f32;
            let reset = bucket["resetTime"].as_str().and_then(parse_rfc3339);
            out.push((format!("{name} · {window}"), used, reset));
        }
    }
    out
}

/// Worst (lowest remaining) quota per family: [Gemini, Claude, GPT-OSS, Gemini 4 Argon],
/// as (used %, reset time). Argon has its own bucket, so it is kept out of Gemini.
fn group(models: &Value) -> [Option<(f32, Option<i64>)>; 4] {
    let mut worst: [Option<(f64, Option<i64>)>; 4] = [None; 4];
    let Some(map) = models.as_object() else {
        return [None; 4];
    };
    for (id, model) in map {
        if model["displayName"].as_str().is_none() || id.contains("image") {
            continue;
        }
        let slot = if id.starts_with("gemini-4-argon") {
            3
        } else if id.starts_with("gemini") {
            0
        } else if id.starts_with("claude") {
            1
        } else if id.starts_with("gpt") {
            2
        } else {
            continue;
        };
        let quota = &model["quotaInfo"];
        let Some(remaining) = quota["remainingFraction"].as_f64() else {
            continue;
        };
        let reset = quota["resetTime"].as_str().and_then(parse_rfc3339);
        if worst[slot].map_or(true, |(r, _)| remaining < r) {
            worst[slot] = Some((remaining, reset));
        }
    }
    worst.map(|w| {
        w.map(|(remaining, reset)| {
            #[allow(clippy::cast_possible_truncation)]
            let used = ((1.0 - remaining.clamp(0.0, 1.0)) * 100.0) as f32;
            (used, reset)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_take_the_lowest_remaining_per_family() {
        let models = json!({
            "gemini-3-flash": {"displayName": "G3", "quotaInfo": {"remainingFraction": 0.8}},
            "gemini-4-argon-high": {"displayName": "G4", "quotaInfo": {"remainingFraction": 0.25}},
            "gemini-3.1-flash-image": {"displayName": "Img", "quotaInfo": {"remainingFraction": 0.0}},
            "claude-opus-5-5-high": {"displayName": "Opus", "quotaInfo": {"remainingFraction": 1.0}},
            "chat_1": {"quotaInfo": {"remainingFraction": 0.0}}
        });
        let g = group(&models);
        assert_eq!(g[0].as_ref().map(|x| x.0.round()), Some(20.0));
        assert_eq!(g[1].as_ref().map(|x| x.0), Some(0.0));
        assert!(g[2].is_none());
        assert_eq!(g[3].as_ref().map(|x| x.0.round()), Some(75.0));
    }

    #[test]
    fn requests_carry_the_agy_cli_identity() {
        let ua = user_agent();
        assert!(ua.starts_with("antigravity/cli/1.5.9 (aidev_client;"));
        assert!(ua.contains("auth_method=consumer"));
        if cfg!(all(windows, target_arch = "x86_64")) {
            assert_eq!(platform_enum(), 5);
        }
    }

    #[test]
    fn summary_groups_become_bars() {
        let v = json!({"groups": [{"displayName": "Claude and GPT models", "buckets": [
            {"window": "weekly", "remainingFraction": 0.53},
            {"window": "5h", "remainingFraction": 1},
            {"window": "5h", "disabled": true, "remainingFraction": 0}
        ]}, {"displayName": "Gemini 4 Argon", "buckets": [
            {"displayName": "Weekly Limit", "remainingFraction": 0.8227}
        ]}]});
        let bars = summary_bars(&v);
        assert_eq!(bars.len(), 3);
        assert_eq!(bars[0].0, "Claude & GPT · weekly");
        assert_eq!(bars[0].1.round(), 47.0);
        assert_eq!(bars[2].0, "Gemini 4 Argon · Weekly Limit");
    }

    #[test]
    fn remembered_numbers_show_as_of_and_passed_resets_read_unused() {
        let now = now_secs();
        let bars = vec![
            ("Gemini · weekly".to_owned(), 40.0, Some(now + 3600)),
            ("Claude · 5-hour".to_owned(), 90.0, Some(now - 60)),
        ];
        let row = shown(
            "agy:sek2",
            "Antigravity · Sek2",
            "Pro".into(),
            &bars,
            Some(now - 1500),
        );
        assert_eq!(row.error, "");
        assert_eq!(row.bars[1].1, 0.0);
        assert!(row.bars[0].2.starts_with("Resets in"));
        assert!(row.summary.ends_with("(as of 25m ago)"));
    }
}
