#![allow(clippy::unwrap_used)]
use super::*;
use chrono::{Datelike, Local, TimeZone, Timelike};

#[test]
fn claude_five_hour_limit_is_quota_with_reset() {
    let info = AgentErrorInfo {
        code: Some(-32603),
        message: "Internal error: You've hit your 5-hour limit · resets 10:40am (Asia/Bangkok)"
            .into(),
        data: None,
    };
    match classify_generic(&info, ErrorDialect::ClaudeCode) {
        ErrorClass::QuotaExhausted { resets_at } => {
            let at = Local.timestamp_opt(resets_at.unwrap(), 0).unwrap();
            assert_eq!((at.hour(), at.minute()), (10, 40));
        }
        other => panic!("expected quota, got {other:?}"),
    }
}

#[test]
fn clock_reset_rolls_to_tomorrow_when_passed() {
    let now = Local.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
    let at = parse_clock_from_rest("10:40am", now).unwrap();
    assert_eq!(
        at,
        Local
            .with_ymd_and_hms(2026, 11, 16, 10, 40, 0)
            .unwrap()
            .timestamp()
    );
    let pm = parse_clock_from_rest("3pm", now).unwrap();
    assert_eq!(
        pm,
        Local
            .with_ymd_and_hms(2026, 11, 15, 15, 0, 0)
            .unwrap()
            .timestamp()
    );
    // Recent time within 15 minutes resets immediately rather than jumping to tomorrow.
    let recent = Local.with_ymd_and_hms(2026, 11, 15, 12, 5, 0).unwrap();
    let at_recent = parse_clock_from_rest("12:01pm", recent).unwrap();
    assert_eq!(at_recent, recent.timestamp());
}

/// 固定"现在"为 10:00：真实时钟落在 13:31–13:46 时，"刚过去 15 分钟内"规则会返回当前时间，测试随运行时刻失败。
fn assert_clock_at_1331(message: &str) {
    let now = Local.with_ymd_and_hms(2026, 11, 24, 10, 0, 0).unwrap();
    let at = parse_reset_from_text_at(message, now.timestamp()).unwrap();
    let dt = Local.timestamp_opt(at, 0).unwrap();
    assert_eq!((dt.hour(), dt.minute()), (13, 31));
}

#[test]
fn codex_try_again_at_parsed() {
    let info = AgentErrorInfo {
        code: None,
        message: "You’ve hit your usage limit. Upgrade to Pro, or try again at 1:31 PM.".into(),
        data: None,
    };
    match classify_generic(&info, ErrorDialect::Codex) {
        ErrorClass::QuotaExhausted { resets_at: Some(_) } => assert_clock_at_1331(&info.message),
        other => panic!("expected quota with reset, got {other:?}"),
    }
}

#[test]
fn codex_try_again_at_with_date_and_url_parsed() {
    let info = AgentErrorInfo {
        code: None,
        message: "You’ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/settings/usage to purchase more credits or try again at 1:31 PM.\n\n".into(),
        data: None,
    };
    match classify_generic(&info, ErrorDialect::Codex) {
        ErrorClass::QuotaExhausted { resets_at: Some(_) } => assert_clock_at_1331(&info.message),
        other => panic!("expected quota with reset, got {other:?}"),
    }
}

#[test]
fn codex_try_again_at_with_date_parsed() {
    let now = Local.with_ymd_and_hms(2026, 11, 24, 10, 0, 0).unwrap();
    let at = parse_clock_from_rest("Nov 24th, 2026 1:31 PM.", now).unwrap();
    let dt = Local.timestamp_opt(at, 0).unwrap();
    assert_eq!(
        (dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute()),
        (2026, 11, 24, 13, 31)
    );
}

#[test]
fn codex_try_again_in_relative_duration() {
    let text = "You've hit your usage limit. Try again in 20 minutes.";
    let parsed = parse_reset_from_text(text).unwrap();
    let now = now_secs();
    assert!(parsed >= now + 1200 - 5);
    assert!(parsed <= now + 1200 + 5);
}

#[test]
fn codex_24h_clock_parsed() {
    let now = Local.with_ymd_and_hms(2026, 11, 24, 10, 0, 0).unwrap();
    let at = parse_clock_from_rest("18:15", now).unwrap();
    let dt = Local.timestamp_opt(at, 0).unwrap();
    assert_eq!((dt.hour(), dt.minute()), (18, 15));
}

#[test]
fn codex_exhaustion_without_time_falls_back_to_5h() {
    let info = AgentErrorInfo {
        code: None,
        message: "You've hit your usage limit.".into(),
        data: None,
    };
    let before = now_secs();
    match classify_generic(&info, ErrorDialect::Codex) {
        ErrorClass::QuotaExhausted {
            resets_at: Some(at),
        } => {
            assert!(at >= before + 5 * 3600 - 5);
            assert!(at <= before + 5 * 3600 + 5);
        }
        other => panic!("expected quota with 5h fallback, got {other:?}"),
    }
}
