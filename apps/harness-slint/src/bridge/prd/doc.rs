//! `prd.json` reading for the PRD page: lenient parsing (missing fields show as empty, unknown statuses
//! as planned), status roll-up per phase, and the `docs/specs/*/prd.json` scan of a workspace.

use std::path::Path;

use serde_json::Value;

#[derive(Debug, Default, PartialEq)]
pub(super) struct Node {
    pub(super) phase: usize,
    pub(super) feature: bool,
    pub(super) id: String,
    pub(super) title: String,
    pub(super) status: String,
    pub(super) req: String,
    pub(super) task: String,
    pub(super) agent: String,
    pub(super) files: String,
    pub(super) done_when: String,
    pub(super) depends: String,
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Phase {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) status: String,
    pub(super) done: usize,
    pub(super) total: usize,
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Doc {
    pub(super) title: String,
    pub(super) spec_path: String,
    pub(super) gate_mode: String,
    pub(super) gate_passed: bool,
    pub(super) gate_issues: usize,
    pub(super) phases: Vec<Phase>,
    pub(super) nodes: Vec<Node>,
}

#[derive(Default)]
pub(super) struct Loaded {
    pub(super) specs: Vec<(String, String)>,
    pub(super) selected: usize,
    pub(super) doc: Doc,
    pub(super) error: String,
}

/// A string field, an array of strings joined with ", ", a number or a bool as text; "" otherwise.
fn text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(s) => Some(s.trim().to_owned()),
                Value::Null => None,
                other => Some(other.to_string()),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn status(value: &Value) -> String {
    let status = text(value, "status").to_lowercase();
    match status.as_str() {
        "done" | "completed" | "complete" => "done".into(),
        "running" | "in-progress" | "in_progress" | "inprogress" => "running".into(),
        "blocked" | "failed" => "blocked".into(),
        _ => "planned".into(),
    }
}

fn task_node(phase: usize, item: &Value) -> Node {
    Node {
        phase,
        feature: false,
        id: text(item, "id"),
        title: text(item, "title"),
        status: status(item),
        req: text(item, "reqId"),
        task: text(item, "taskId"),
        agent: text(item, "assignedAgent"),
        files: text(item, "targetFiles"),
        done_when: text(item, "doneWhen"),
        depends: text(item, "dependsOn"),
    }
}

/// Phase status from its tasks when prd.json does not say: done when all are done, else running/blocked
/// when any is, else planned.
fn rollup(tasks: &[&Node]) -> String {
    if !tasks.is_empty() && tasks.iter().all(|t| t.status == "done") {
        "done".into()
    } else if tasks.iter().any(|t| t.status == "running") {
        "running".into()
    } else if tasks.iter().any(|t| t.status == "blocked") {
        "blocked".into()
    } else {
        "planned".into()
    }
}

pub(super) fn parse(json: &str) -> Result<Doc, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| format!("prd.json: {e}"))?;
    let gate = root.get("qualityGate").cloned().unwrap_or(Value::Null);
    let mut doc = Doc {
        title: text(&root, "title"),
        spec_path: text(&root, "specFilePath"),
        gate_mode: text(&gate, "mode").to_lowercase(),
        gate_passed: gate.get("passed").and_then(Value::as_bool).unwrap_or(false),
        gate_issues: list(&gate, "issues").len(),
        ..Doc::default()
    };
    for (pi, phase) in list(&root, "phases").iter().enumerate() {
        let first = doc.nodes.len();
        for feature in list(phase, "features") {
            let subs = list(feature, "subfeatures");
            if subs.is_empty() {
                // A feature without sub-features is itself the task.
                doc.nodes.push(task_node(pi, feature));
                continue;
            }
            doc.nodes.push(Node {
                phase: pi,
                feature: true,
                id: text(feature, "id"),
                title: text(feature, "title"),
                ..Node::default()
            });
            doc.nodes.extend(subs.iter().map(|sub| task_node(pi, sub)));
        }
        let tasks: Vec<&Node> = doc.nodes[first..].iter().filter(|n| !n.feature).collect();
        let explicit = text(phase, "status");
        doc.phases.push(Phase {
            id: text(phase, "id"),
            title: text(phase, "title"),
            status: if explicit.is_empty() || explicit == "ready" {
                rollup(&tasks)
            } else {
                status(phase)
            },
            done: tasks.iter().filter(|t| t.status == "done").count(),
            total: tasks.len(),
        });
    }
    Ok(doc)
}

/// `<root>/docs/specs/<slug>/prd.json` files as (slug, title), sorted by slug.
fn scan(root: &Path) -> Vec<(String, String)> {
    let Ok(dirs) = std::fs::read_dir(root.join("docs").join("specs")) else {
        return Vec::new();
    };
    let mut specs: Vec<(String, String)> = dirs
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("prd.json").is_file())
        .map(|entry| {
            let slug = entry.file_name().to_string_lossy().into_owned();
            let title = std::fs::read_to_string(entry.path().join("prd.json"))
                .ok()
                .and_then(|json| serde_json::from_str::<Value>(&json).ok())
                .map(|v| text(&v, "title"))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| slug.clone());
            (slug, title)
        })
        .collect();
    specs.sort();
    specs
}

pub(super) fn load(root: &Path, want: &str) -> Loaded {
    let specs = scan(root);
    let selected = specs.iter().position(|(slug, _)| slug == want).unwrap_or(0);
    let mut out = Loaded {
        selected,
        ..Loaded::default()
    };
    if let Some((slug, _)) = specs.get(selected) {
        let path = root.join("docs").join("specs").join(slug).join("prd.json");
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|j| parse(&j))
        {
            Ok(doc) => out.doc = doc,
            Err(error) => out.error = format!("{}: {error}", path.display()),
        }
        if out.doc.spec_path.is_empty() {
            out.doc.spec_path = format!("docs/specs/{slug}/prd.md");
        }
    }
    out.specs = specs;
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::parse;

    #[test]
    fn parses_the_plan_schema_and_rolls_up_status() {
        let doc = parse(
            r#"{"title":"Shop","specFilePath":"docs/specs/shop/prd.md",
            "qualityGate":{"mode":"Strict","passed":false,"issues":[{"severity":"high","text":"x"}]},
            "phases":[{"id":"phase-1","title":"Catalog","status":"ready","features":[
              {"id":"f1","title":"Menu","subfeatures":[
                {"id":"s1","title":"List","reqId":"REQ-001","taskId":"TASK-001","status":"done",
                 "assignedAgent":"codex","targetFiles":["src/a.rs","src/b.rs"],"doneWhen":"tests pass"},
                {"id":"s2","title":"Detail","status":"in_progress","dependsOn":["TASK-001"]}]},
              {"id":"f2","title":"Search","status":"weird"}]}]}"#,
        )
        .unwrap();
        assert_eq!(doc.title, "Shop");
        assert_eq!(doc.gate_mode, "strict");
        assert!(!doc.gate_passed);
        assert_eq!(doc.gate_issues, 1);
        assert_eq!(doc.phases.len(), 1);
        let phase = &doc.phases[0];
        assert_eq!((phase.done, phase.total), (1, 3));
        assert_eq!(phase.status, "running");
        assert_eq!(doc.nodes.len(), 4);
        assert!(doc.nodes[0].feature);
        assert_eq!(doc.nodes[1].files, "src/a.rs, src/b.rs");
        assert_eq!(doc.nodes[1].agent, "codex");
        assert_eq!(doc.nodes[2].status, "running");
        assert_eq!(doc.nodes[2].depends, "TASK-001");
        assert_eq!(doc.nodes[3].status, "planned");
        assert!(!doc.nodes[3].feature);
    }

    #[test]
    fn bad_json_is_an_error() {
        assert!(parse("{").is_err());
        assert_eq!(parse("{}").unwrap().phases, Vec::new());
    }
}
