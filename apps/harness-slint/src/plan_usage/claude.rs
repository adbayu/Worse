//! Claude plan usage of one account: `/api/oauth/usage` with the profile's own CLI login.

use serde_json::Value;

use super::{failed, now_ms, title_case, window_of, Account, Fetched, USAGE_URL};

pub(super) async fn fetch_one(client: &reqwest::Client, account: Account) -> Fetched {
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
