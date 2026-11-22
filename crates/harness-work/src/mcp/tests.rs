use serde_json::json;

use super::*;

fn first_text(v: &Value) -> String {
    v["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn create_validate_sheet_and_render() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Server::new(tmp.path().to_path_buf());
    let made = s.call(
        "work_create",
        &json!({"kind": "sheet", "title": "Budget", "template": "budget"}),
    );
    assert!(made.get("isError").is_none(), "{made}");
    assert_eq!(
        first_text(&s.call("work_validate", &json!({"path": "budget"}))),
        "No issues."
    );
    let read = s.call(
        "work_sheet",
        &json!({"path": "budget", "set": {"B2": 60000}, "read": "Budget!B7:B7"}),
    );
    assert!(first_text(&read).contains("83,000"), "{read}");
    let list = first_text(&s.call("work_sheet", &json!({"path": "budget", "read": "*"})));
    assert!(list.contains("- Budget: A1:"), "{list}");
    let found = first_text(&s.call("work_sheet", &json!({"path": "budget", "find": "60000"})));
    assert!(found.contains("Budget!B2"), "{found}");
    let whole = first_text(&s.call("work_sheet", &json!({"path": "budget", "read": "Budget"})));
    assert!(whole.contains("B7=83,000"), "{whole}");
    let shot = s.call("work_render", &json!({"path": "work/budget/book.json"}));
    assert_eq!(shot["content"][2]["type"], "image");
    assert!(s
        .call("work_render", &json!({"path": "nope"}))
        .get("isError")
        .is_some());
}

#[test]
fn lists_tools() {
    let names: Vec<String> = tools::definitions()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"work_export".to_string()));
}

#[test]
fn sheet_ops_fill_trace_errors_pairs_and_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Server::new(tmp.path().to_path_buf());
    s.call(
        "work_create",
        &json!({"kind": "sheet", "title": "Ops", "template": "budget"}),
    );
    let fill = s.call(
        "work_sheet",
        &json!({
            "path": "ops",
            "set": {"Budget!D2": "=B2*2", "Budget!E2": "=1/0"},
            "fill": {"from": "Budget!D2", "to": "D3:D4"},
            "style": {"Budget!D2:D4": {"bold": true, "format": "#,##0"}},
            "errors": true,
            "trace": "Budget!B7"
        }),
    );
    assert!(fill.get("isError").is_none(), "{fill}");
    let out = first_text(&fill);
    assert!(out.contains("Saved."), "{out}");
    assert!(out.contains("#DIV/0!"), "{out}");
    assert!(out.contains("Reads:"), "{out}");

    let pairs = first_text(&s.call(
        "work_sheet",
        &json!({"path": "ops", "read": "Budget!A1:B4", "mode": "pairs"}),
    ));
    assert!(pairs.contains("→"), "{pairs}");

    let f_only = first_text(&s.call(
        "work_sheet",
        &json!({"path": "ops", "find": "SUM", "in": "formulas"}),
    ));
    assert!(f_only.contains("Budget!B7"), "{f_only}");

    let ins = s.call(
        "work_sheet",
        &json!({"path": "ops", "insert_rows": "Budget!3:3"}),
    );
    assert!(ins.get("isError").is_none(), "{ins}");
}
