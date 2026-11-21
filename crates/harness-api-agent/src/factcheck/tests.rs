#![allow(clippy::unwrap_used)]

use super::*;

fn two_claims() -> Value {
    json!([
        {"text": "Cost_share rose 5% in 2024 & 2025.", "location": "Results", "verdict": "supported",
         "sources": [{"url": "https://example.org/a_b", "title": "A & B"}],
         "queries": ["cost share 2024"]},
        {"text": "Latency doubled.", "location": "Results", "verdict": "notSupported"}
    ])
}

#[test]
fn write_renders_json_and_an_escaped_appendix() {
    let out = run(
        &json!({"action": "write", "question": "q", "report": "r", "dir": "research/x",
                          "claims": two_claims()}),
    )
    .unwrap();
    assert!(out.contains("\"supported\": 1") && out.contains("\"notSupported\": 1"));
    assert!(out.contains("research/x/factcheck.json"));
    assert!(out.contains("1 of 2 claims supported, 1 not supported, 0 unclear"));
    assert!(out.contains("Cost\\_share rose 5\\% in 2024 \\& 2025."));
    assert!(out.contains("\\url{https://example.org/a b (A \\& B)}"));
    assert!(out.contains("no source"));
    assert_eq!(out.matches("\\section*{Fact-check}").count(), 1);
    assert!(out.contains("\\begin{longtable}") && out.trim_end().ends_with("\\end{longtable}"));
}

#[test]
fn batched_records_add_up_and_write_needs_no_claims() {
    let q = "batched question";
    let first = json!([{"text": "Alpha grew 5% in 2024.", "location": "R", "verdict": "supported",
                        "sources": [{"url": "https://example.org/a", "title": "A"}]}]);
    let second =
        json!([{"text": "Beta fell in 2025.", "location": "R", "verdict": "unclear"}]).to_string();
    run(&json!({"action": "record", "question": q, "claims": first})).unwrap();
    let out = run(&json!({"action": "record", "question": q, "claims": second})).unwrap();
    assert!(out.contains("1 of 2 claims supported"));
    let written = run(&json!({"action": "write", "question": q, "dir": "research/b"})).unwrap();
    assert!(written.contains("Alpha grew") && written.contains("Beta fell"));
}

#[test]
fn write_with_a_folder_saves_the_json_and_appends_the_table_once() {
    let root = std::env::temp_dir().join(format!("fc-write-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("report.tex"),
        "\\begin{document}\nBody\n\\end{document}\n",
    )
    .unwrap();
    let args = json!({"action": "write", "question": "folder q", "dir": "research/x", "claims": two_claims()});
    assert!(writes(&args));
    let out = run_at(&args, Some(&root)).unwrap();
    assert!(out.contains("1 of 2 claims supported"));
    assert!(root.join("factcheck.json").is_file());
    run_at(&args, Some(&root)).unwrap();
    let tex = std::fs::read_to_string(root.join("report.tex")).unwrap();
    assert_eq!(tex.matches("\\section*{Fact-check}").count(), 1);
    assert!(tex.trim_end().ends_with("\\end{document}"));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn record_lists_the_unsupported_claims_and_asks_for_one_revision() {
    let out = run(&json!({"action": "record", "claims": two_claims()})).unwrap();
    assert!(out.contains("1 of 2 claims supported"));
    assert!(out.contains("Latency doubled."));
    assert!(!out.contains("Cost_share"));
    assert!(out.contains("Revise the 1 not-supported claim(s) once"));
}

#[test]
fn extract_returns_numbered_claims_with_locations() {
    let out = run(&json!({"action": "extract", "question": "growth",
                          "report": "\\section{Results}\nCosts fell 5 percent in 2024 across the fleet.\n"}))
    .unwrap();
    assert!(out.starts_with("1 atomic claims"));
    assert!(out.contains("1. [Results, sentence 1] Costs fell 5 percent in 2024 across the fleet."));
    assert!(out.contains("action \"record\""));
}

#[test]
fn dispatch_guards_the_actions() {
    let def = definition();
    assert_eq!(def["function"]["name"], json!(NAME));
    assert!(handles("fact_check") && !handles("factcheck"));
    assert_eq!(title(&json!({"action": "record"})), "Score claims");
    assert_eq!(title(&json!({"action": "extract"})), "Extract claims");
    assert_eq!(title(&json!({})), "Extract claims");
    assert!(run(&json!({"action": "extract", "report": ""})).is_err());
    assert!(run(&json!({"action": "write", "claims": []})).is_err());
    assert!(run(&json!({"action": ""})).is_err());
    assert!(run(&json!({"action": "nope"})).is_err());
}

#[test]
fn escape_covers_every_special_character() {
    assert_eq!(escape("a_b & 50% #1 $5"), "a\\_b \\& 50\\% \\#1 \\$5");
    assert_eq!(escape("a\\b"), "a\\textbackslash{}b");
}
