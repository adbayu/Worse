//! Turning fetched usage into UI rows, plus time helpers.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use slint::{ModelRc, SharedString, VecModel};

use super::Fetched;
use crate::ui::{PlanBar, PlanLimit};

pub(super) fn window_of(v: &Value) -> Option<(f32, String)> {
    #[allow(clippy::cast_possible_truncation)]
    let used = v["utilization"].as_f64()? as f32;
    let reset = v["resets_at"]
        .as_str()
        .and_then(parse_rfc3339)
        .map(resets_in)
        .unwrap_or_default();
    Some((used.clamp(0.0, 100.0), reset))
}

pub(super) fn to_row(f: Fetched) -> PlanLimit {
    let known = f.five.is_some() || f.week.is_some();
    let (five, five_reset) = f.five.unwrap_or_default();
    let (week, week_reset) = f.week.unwrap_or_default();
    let (opus, opus_reset) = f.opus.clone().unwrap_or_default();
    let reset_count = f.reset_count.unwrap_or(-1);
    let reset_text = match reset_count {
        0 => "No saved resets available".to_owned(),
        1 => "1 saved reset available".to_owned(),
        n if n > 1 => format!("{n} saved resets available"),
        _ => String::new(),
    };
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
        reset_count,
        reset_text: sv(&reset_text),
        reset_eligible: f.reset_eligible,
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

pub(super) fn sv(text: &str) -> SharedString {
    SharedString::from(text)
}

pub(super) fn title_case(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

pub(super) fn now_ms() -> u64 {
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
