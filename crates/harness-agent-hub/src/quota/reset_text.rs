//! Reset times in free-form limit messages ("try again in 3h", "resets 5pm", ISO / epoch values).

use super::normalize_epoch;
use crate::util::now_secs;

/// `…|<unix>` suffix (Claude Code) or `resets at <ISO-8601>`, `try again at <clock>`, etc.
pub(crate) fn parse_reset_from_text(text: &str) -> Option<i64> {
    parse_reset_from_text_at(text, now_secs())
}

pub(crate) fn is_codex_usage_limit_notice(text: &str) -> bool {
    let text = text.trim_start().to_ascii_lowercase();
    [
        "you've hit your usage limit",
        "you’ve hit your usage limit",
        "you have hit your usage limit",
        "you've reached your usage limit",
        "you’ve reached your usage limit",
        "you have reached your usage limit",
        "your usage limit has been reached",
        "your account has exceeded the usage limit",
        "usage limit reached",
        "you've hit your session limit",
        "you’ve hit your session limit",
        "you have hit your session limit",
        "you've reached your session limit",
        "you’ve reached your session limit",
        "you have reached your session limit",
        "session limit reached",
        "quota exceeded",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

/// Parses a reset deadline relative to the supplied Unix timestamp.
pub(crate) fn parse_reset_from_text_at(text: &str, now_epoch: i64) -> Option<i64> {
    if let Some((_, tail)) = text.rsplit_once('|') {
        if let Ok(n) = tail.trim().parse::<i64>() {
            return Some(normalize_epoch(n));
        }
    }
    let lower = text.to_ascii_lowercase();
    for marker in [
        "resets at ",
        "resets in ",
        "try again at ",
        "try again after ",
        "try again in ",
        "resets ",
    ] {
        if let Some(index) = lower.find(marker) {
            let rest = &lower[index + marker.len()..];
            if marker.ends_with("in ") {
                if let Some(n) = parse_relative_duration(rest, now_epoch) {
                    return Some(n);
                }
            }
            if let Some(token) = rest.split_whitespace().next() {
                let clean = token.trim_end_matches(&[')', '.', ',', ';'][..]);
                if let Some(n) = parse_epoch_or_iso(clean) {
                    return Some(n);
                }
            }
            let now = chrono::TimeZone::timestamp_opt(&chrono::Local, now_epoch, 0).single()?;
            if let Some(n) = parse_clock_from_rest(rest, now) {
                return Some(n);
            }
        }
    }
    None
}

pub(super) fn parse_relative_duration(rest: &str, now_epoch: i64) -> Option<i64> {
    let mut words = rest.split_whitespace();
    let first = words.next()?.trim_end_matches(&[')', '.', ',', ';'][..]);
    if let Some(unit) = first.chars().last() {
        if unit.is_alphabetic() {
            let num_part = &first[..first.len() - unit.len_utf8()];
            if let Ok(num) = num_part.parse::<i64>() {
                let mult = match unit {
                    's' => 1,
                    'm' => 60,
                    'h' => 3600,
                    'd' => 86400,
                    _ => 0,
                };
                if mult > 0 {
                    return Some(now_epoch + num.saturating_mul(mult));
                }
            }
        }
    }
    let num = first.parse::<i64>().ok()?;
    let second = words.next()?.trim_end_matches(&[')', '.', ',', ';'][..]);
    let mult = match second {
        s if s.starts_with("sec") || s == "s" => 1,
        s if s.starts_with("min") || s == "m" => 60,
        s if s.starts_with("hour") || s == "hr" || s == "hrs" || s == "h" => 3600,
        s if s.starts_with("day") || s == "d" => 86400,
        _ => return None,
    };
    Some(now_epoch + num.saturating_mul(mult))
}

#[allow(clippy::many_single_char_names)]
pub(super) fn parse_clock_from_rest(
    rest: &str,
    now: chrono::DateTime<chrono::Local>,
) -> Option<i64> {
    use chrono::{Datelike, NaiveDate, NaiveTime, TimeZone};

    let months = [
        ("jan", 1u32),
        ("feb", 2),
        ("mar", 3),
        ("apr", 4),
        ("may", 5),
        ("jun", 6),
        ("jul", 7),
        ("aug", 8),
        ("sep", 9),
        ("oct", 10),
        ("nov", 11),
        ("dec", 12),
    ];

    let clean = rest.replace([',', ';', '.'], " ");
    let words: Vec<&str> = clean.split_whitespace().collect();

    let mut month = None;
    let mut day = None;
    let mut year = None;
    let mut clock_token = None;

    let mut i = 0;
    while i < words.len() {
        let w = words[i].to_ascii_lowercase();
        if month.is_none() {
            for (prefix, m_val) in months {
                if w.starts_with(prefix) {
                    month = Some(m_val);
                    if i + 1 < words.len() {
                        let next = words[i + 1].trim_end_matches(|c: char| c.is_alphabetic());
                        if let Ok(d) = next.parse::<u32>() {
                            if (1..=31).contains(&d) {
                                day = Some(d);
                                i += 1;
                                if i + 1 < words.len() {
                                    if let Ok(y) = words[i + 1].parse::<i32>() {
                                        if (2020..=2100).contains(&y) {
                                            year = Some(y);
                                            i += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    break;
                }
            }
            if month.is_some() {
                i += 1;
                continue;
            }
        }

        if clock_token.is_none() && (w.contains(':') || w.ends_with("am") || w.ends_with("pm")) {
            if w.ends_with("am") || w.ends_with("pm") || !w.contains(':') {
                clock_token = Some(w);
            } else if i + 1 < words.len() {
                let next = words[i + 1].to_ascii_lowercase();
                if next == "am" || next == "pm" {
                    clock_token = Some(format!("{w}{next}"));
                } else {
                    clock_token = Some(w);
                }
            } else {
                clock_token = Some(w);
            }
            break;
        }

        i += 1;
    }

    let clock_str = clock_token?;
    let (h, m) = parse_clock(&clock_str)?;
    let time = NaiveTime::from_hms_opt(h, m, 0)?;

    let target_date = match (month, day) {
        (Some(mo), Some(d)) => {
            let y = year.unwrap_or_else(|| now.year());
            NaiveDate::from_ymd_opt(y, mo, d)?
        }
        _ => now.date_naive(),
    };

    let mut at = chrono::Local
        .from_local_datetime(&target_date.and_time(time))
        .earliest()?;

    if month.is_none() && at < now {
        if (now - at).num_minutes() <= 15 {
            at = now;
        } else {
            at += chrono::Duration::days(1);
        }
    }

    Some(at.timestamp())
}

pub(super) fn parse_clock(token: &str) -> Option<(u32, u32)> {
    let token = token
        .trim_end_matches(&[')', '.', ',', ';'][..])
        .to_ascii_lowercase();
    let (clock, pm) = if let Some(c) = token.strip_suffix("pm") {
        (c.trim(), Some(true))
    } else if let Some(c) = token.strip_suffix("am") {
        (c.trim(), Some(false))
    } else {
        (token.as_str(), None)
    };
    let (hour, minute) = match clock.split_once(':') {
        Some((h, m)) => (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?),
        None => (clock.trim().parse::<u32>().ok()?, 0),
    };
    if minute >= 60 {
        return None;
    }
    let hour = match pm {
        Some(true) => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            (hour % 12) + 12
        }
        Some(false) => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour % 12
        }
        None => {
            if hour >= 24 {
                return None;
            }
            hour
        }
    };
    Some((hour, minute))
}

pub(super) fn parse_epoch_or_iso(s: &str) -> Option<i64> {
    if let Ok(n) = s.trim().parse::<i64>() {
        return Some(normalize_epoch(n));
    }
    parse_iso8601_utc(s.trim())
}

/// Minimal `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)` parser.
pub(super) fn parse_iso8601_utc(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> { s.get(range)?.parse::<i64>().ok() };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = s.get(19..)?;
    if let Some(stripped) = rest.strip_prefix('.') {
        let digits = stripped.chars().take_while(char::is_ascii_digit).count();
        rest = stripped.get(digits..)?;
    }
    let offset = if rest.is_empty() || rest == "Z" || rest == "z" {
        0
    } else {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let h = rest.get(1..3)?.parse::<i64>().ok()?;
        let m = rest.get(4..6)?.parse::<i64>().ok()?;
        sign * (h * 3600 + m * 60)
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Howard Hinnant's days-from-civil.
pub(super) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
