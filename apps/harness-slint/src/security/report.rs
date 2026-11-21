//! `osv-scanner scan --format json` report parsing (spec `docs/specs/harness/security-panel.md` §3.3): one row per
//! (package, advisory), sorted by severity then package name.

use serde_json::Value;

/// One advisory of one package, as the Security page and the commit gate read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vuln {
    pub package: String,
    pub version: String,
    pub ecosystem: String,
    pub id: String,
    pub severity: String,
    pub summary: String,
    pub fixed: String,
    pub source: String,
}

impl Vuln {
    /// 3 critical, 2 high, 1 medium, 0 low/unknown — the sort and count key.
    #[must_use]
    pub fn rank(&self) -> i32 {
        match self.severity.as_str() {
            "critical" => 3,
            "high" => 2,
            "medium" => 1,
            _ => 0,
        }
    }
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// `database_specific.severity` first, else the CVSS v3/v4 vector in `severity[]`, else `unknown`. A bare numeric
/// score is returned as-is so `score()` can bucket it and `group_score()` can still fill a missing one.
fn severity(advisory: &Value) -> String {
    let named = advisory
        .get("database_specific")
        .map(|specific| text(specific, "severity"))
        .unwrap_or_default();
    if !named.is_empty() {
        return normalized(&named);
    }
    advisory
        .get("severity")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| text(entry, "type").starts_with("CVSS"))
        .find_map(|entry| cvss_score(&text(entry, "score")))
        .map_or_else(String::new, |score| score.to_string())
}

/// Lowercase label, `moderate` folded into `medium`; anything else is returned unchanged so the caller can decide.
fn normalized(value: &str) -> String {
    let lower = value.trim().to_ascii_lowercase();
    if lower == "moderate" {
        "medium".to_owned()
    } else {
        lower
    }
}

/// Buckets a CVSS base score into the four labels; `None` for anything that is not a number.
fn score(value: &str) -> Option<String> {
    let score = value.parse::<f64>().ok()?;
    let base = if score > 10.0 { score / 10.0 } else { score };
    Some(
        if base >= 9.0 {
            "critical"
        } else if base >= 7.0 {
            "high"
        } else if base >= 4.0 {
            "medium"
        } else {
            "low"
        }
        .to_owned(),
    )
}

/// CVSS base score of an advisory: a bare number, or the `base_score:<n>` part some databases append.
fn cvss_score(value: &str) -> Option<f64> {
    let number = value.trim().parse::<f64>().ok().or_else(|| {
        value
            .split('/')
            .filter_map(|part| part.trim().strip_prefix("base_score:"))
            .find_map(|part| part.trim().parse::<f64>().ok())
    })?;
    Some(if number > 10.0 { number / 10.0 } else { number })
}

/// `groups[].max_severity` of the group that lists `id` among its ids or aliases.
fn group_score(groups: &[Value], id: &str) -> Option<String> {
    groups
        .iter()
        .find(|group| {
            ["ids", "aliases"].iter().any(|key| {
                group
                    .get(*key)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .any(|listed| listed.eq_ignore_ascii_case(id))
            })
        })
        .map(|group| text(group, "max_severity"))
        .filter(|value| !value.is_empty())
        .and_then(|value| score(&value))
}

/// Numeric parts of a version (`v1.2.3-rc1` → `[1, 2, 3]`), for ordering fixed versions.
fn version_key(value: &str) -> Vec<u64> {
    value
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or_default()
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// The `fixed` version of `package` that is the lowest one above `version`, empty when no fix exists yet.
fn fixed(advisory: &Value, package: &str, version: &str) -> String {
    let affected: Vec<&Value> = advisory
        .get("affected")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    // 修复：一条公告可能列出多个包（如 @modelcontextprotocol/sdk 与 /client），原来取第一个 fixed，
    // 导致 client 2.0.0 显示 "1.31.0"。只看与该行包名相同的 affected 条目，找不到时才退回全部。
    let own: Vec<&Value> = affected
        .iter()
        .copied()
        .filter(|entry| {
            text(entry.get("package").unwrap_or(&Value::Null), "name").eq_ignore_ascii_case(package)
        })
        .collect();
    let entries = if own.is_empty() { affected } else { own };
    let mut versions: Vec<String> = entries
        .into_iter()
        .filter_map(|entry| entry.get("ranges").and_then(Value::as_array))
        .flatten()
        .filter_map(|range| range.get("events").and_then(Value::as_array))
        .flatten()
        .filter_map(|event| event.get("fixed").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();
    versions.sort_by_key(|value| version_key(value));
    let current = version_key(version);
    versions
        .iter()
        .find(|value| version_key(value) > current)
        .or_else(|| versions.first())
        .cloned()
        .unwrap_or_default()
}

/// Parses a whole report. A malformed document is an error; a report without findings is empty, not an error.
pub fn parse(bytes: &[u8]) -> Result<Vec<Vuln>, String> {
    let root: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("unexpected scan output: {e}"))?;
    let mut rows = Vec::new();
    let results = root
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for result in &results {
        let source = result
            .get("source")
            .map(|value| {
                value
                    .get("path")
                    .and_then(Value::as_str)
                    .map_or_else(|| text(value, "type"), str::to_owned)
            })
            .unwrap_or_default();
        let packages = result
            .get("packages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for entry in &packages {
            let package = entry.get("package").cloned().unwrap_or(Value::Null);
            let advisories = entry
                .get("vulnerabilities")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // osv-scanner puts the numeric CVSS score of a group of aliases in `groups[].max_severity`, while a
            // RustSec advisory usually carries no `database_specific.severity` and no CVSS vector of its own. Without
            // this the whole table would read "unknown".
            let groups = entry
                .get("groups")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for advisory in &advisories {
                let id = text(advisory, "id");
                if id.is_empty() {
                    continue;
                }
                let named = severity(advisory);
                let severity = match named.as_str() {
                    "" => group_score(&groups, &id).unwrap_or_else(|| "unknown".to_owned()),
                    "critical" | "high" | "medium" | "low" | "unknown" => named,
                    numeric => score(numeric)
                        .or_else(|| group_score(&groups, &id))
                        .unwrap_or_else(|| "unknown".to_owned()),
                };
                rows.push(Vuln {
                    package: text(&package, "name"),
                    version: text(&package, "version"),
                    ecosystem: text(&package, "ecosystem"),
                    id,
                    severity,
                    summary: text(advisory, "summary"),
                    fixed: fixed(
                        advisory,
                        &text(&package, "name"),
                        &text(&package, "version"),
                    ),
                    source: source.clone(),
                });
            }
        }
    }
    sort(&mut rows);
    rows.dedup_by(|a, b| a.id == b.id && a.package == b.package && a.version == b.version);
    Ok(rows)
}

fn sort(rows: &mut [Vuln]) {
    rows.sort_by(|a, b| {
        b.rank()
            .cmp(&a.rank())
            .then_with(|| {
                a.package
                    .to_ascii_lowercase()
                    .cmp(&b.package.to_ascii_lowercase())
            })
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Per-severity counts in `critical, high, medium, low+unknown` order.
#[must_use]
pub fn counts(rows: &[Vuln]) -> [usize; 4] {
    let mut out = [0usize; 4];
    for row in rows {
        out[match row.rank() {
            3 => 0,
            2 => 1,
            1 => 2,
            _ => 3,
        }] += 1;
    }
    out
}

#[cfg(test)]
mod tests;
