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
mod tests {
    use super::*;

    const REPORT: &[u8] = br#"{
      "results": [
        {
          "source": { "path": "/work/Cargo.lock", "type": "lockfile" },
          "packages": [
            {
              "package": { "name": "time", "version": "0.1.44", "ecosystem": "crates.io" },
              "vulnerabilities": [
                {
                  "id": "RUSTSEC-2020-0071",
                  "summary": "Potential segfault",
                  "database_specific": { "severity": "MEDIUM" },
                  "affected": [ { "ranges": [ { "events": [ { "introduced": "0" }, { "fixed": "0.2.23" } ] } ] } ]
                },
                {
                  "id": "RUSTSEC-2020-0159",
                  "summary": "Local time zone soundness",
                  "severity": [ { "type": "CVSS_V3", "score": "CVSS:3.1/AV:L/AC:H/PR:N/UI:N/S:U/C:H/I:H/A:H" } ]
                }
              ]
            }
          ]
        },
        {
          "source": { "path": "/work/package-lock.json", "type": "lockfile" },
          "packages": [
            {
              "package": { "name": "lodash", "version": "4.17.15", "ecosystem": "npm" },
              "vulnerabilities": [
                {
                  "id": "GHSA-35jh-r3h4-6jhm",
                  "summary": "Prototype pollution",
                  "database_specific": { "severity": "CRITICAL" },
                  "affected": [ { "ranges": [ { "events": [ { "fixed": "4.17.19" } ] } ] } ]
                }
              ]
            }
          ]
        }
      ]
    }"#;

    #[test]
    fn parses_rows_sorted_by_severity() {
        let rows = parse(REPORT).unwrap_or_default();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, "GHSA-35jh-r3h4-6jhm");
        assert_eq!(rows[0].severity, "critical");
        assert_eq!(rows[0].fixed, "4.17.19");
        assert_eq!(rows[0].source, "/work/package-lock.json");
        assert_eq!(rows[1].id, "RUSTSEC-2020-0071");
        assert_eq!(rows[1].severity, "medium");
        assert_eq!(rows[1].fixed, "0.2.23");
        assert_eq!(rows[1].ecosystem, "crates.io");
        assert_eq!(rows[2].severity, "unknown");
        assert_eq!(rows[2].fixed, "");
        assert_eq!(counts(&rows), [1, 0, 1, 1]);
    }

    #[test]
    fn empty_or_odd_reports_are_not_errors() {
        assert_eq!(parse(br#"{"results":[]}"#).unwrap_or_default().len(), 0);
        assert_eq!(parse(br"{}").unwrap_or_default().len(), 0);
        assert!(parse(b"not json").is_err());
    }

    #[test]
    fn cvss_scores_bucket_into_severities() {
        let label = |value: &str| {
            let advisory: Value =
                serde_json::json!({ "severity": [{ "type": "CVSS_V3", "score": value }] });
            score(&severity(&advisory)).unwrap_or_else(|| "unknown".to_owned())
        };
        assert_eq!(label("9.8"), "critical");
        assert_eq!(label("7.5"), "high");
        assert_eq!(label("5.0"), "medium");
        assert_eq!(label("2.0"), "low");
        assert_eq!(label("nonsense"), "unknown");
        // A vector alone carries no score: it must not be read as 3.1 from `CVSS:3.1`.
        assert_eq!(
            label("CVSS:3.1/AV:L/AC:H/PR:N/UI:N/S:U/C:H/I:H/A:H"),
            "unknown"
        );
        assert_eq!(
            label("CVSS:3.1/AV:L/AC:H/PR:N/UI:N/S:U/C:H/I:H/A:H/base_score:7.8"),
            "high"
        );
    }

    #[test]
    fn a_numeric_group_score_supplies_the_severity() {
        // Shape of a real `osv-scanner v2.7.1` report: RustSec advisories carry no `database_specific.severity`
        // and no CVSS vector, only `groups[].max_severity` next to the package entry.
        let report = br#"{
          "results": [
            {
              "source": { "path": "/work/Cargo.lock", "type": "lockfile" },
              "packages": [
                {
                  "package": { "name": "bincode", "version": "2.0.1", "ecosystem": "crates.io" },
                  "groups": [
                    { "ids": ["RUSTSEC-2025-0141"], "aliases": ["RUSTSEC-2025-0141"], "max_severity": "6.9" }
                  ],
                  "vulnerabilities": [
                    { "id": "RUSTSEC-2025-0141", "summary": "Bincode is unmaintained",
                      "affected": [ { "ranges": [ { "events": [ { "introduced": "0.0.0-0" } ] } ] } ] }
                  ]
                },
                {
                  "package": { "name": "codex", "version": "1.0.0", "ecosystem": "crates.io" },
                  "groups": [ { "ids": ["RUSTSEC-0000-0001"], "max_severity": "" } ],
                  "vulnerabilities": [ { "id": "RUSTSEC-0000-0001" } ]
                }
              ]
            }
          ]
        }"#;
        let rows = parse(report).unwrap_or_default();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].package, "bincode");
        assert_eq!(rows[0].severity, "medium");
        assert_eq!(rows[1].severity, "unknown");
        assert_eq!(counts(&rows), [0, 0, 1, 1]);
    }

    #[test]
    fn fixed_version_belongs_to_the_row_package() {
        let advisory = serde_json::json!({
            "affected": [
                { "package": { "name": "@modelcontextprotocol/sdk" },
                  "ranges": [ { "events": [ { "introduced": "1.12.0" }, { "fixed": "1.31.0" } ] } ] },
                { "package": { "name": "@modelcontextprotocol/client" },
                  "ranges": [ { "events": [ { "introduced": "2.0.0" }, { "fixed": "2.2.0" } ] } ] }
            ]
        });
        assert_eq!(
            super::fixed(&advisory, "@modelcontextprotocol/client", "2.0.0"),
            "2.2.0"
        );
        assert_eq!(
            super::fixed(&advisory, "@modelcontextprotocol/sdk", "1.29.0"),
            "1.31.0"
        );
        let branches = serde_json::json!({
            "affected": [ { "package": { "name": "tar" }, "ranges": [
                { "events": [ { "introduced": "0" }, { "fixed": "6.2.1" } ] },
                { "events": [ { "introduced": "7.0.0" }, { "fixed": "7.5.19" } ] } ] } ]
        });
        assert_eq!(super::fixed(&branches, "tar", "7.5.13"), "7.5.19");
    }
}
