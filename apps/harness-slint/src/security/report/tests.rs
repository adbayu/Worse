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
