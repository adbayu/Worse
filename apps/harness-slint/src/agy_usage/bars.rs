//! Quota buckets and model families from Cloud Code responses → usage bars.

use serde_json::Value;

use super::RawBar;
use crate::plan_usage::parse_rfc3339;

fn short_group(name: &str) -> String {
    let name = name
        .trim()
        .trim_end_matches(" models")
        .trim_end_matches(" Models");
    name.replace(" and ", " & ")
}

/// Every bucket of every quota group, as (label, used %, reset time), in server order.
pub(super) fn summary_bars(summary: &Value) -> Vec<RawBar> {
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
pub(super) fn group(models: &Value) -> [Option<(f32, Option<i64>)>; 4] {
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
