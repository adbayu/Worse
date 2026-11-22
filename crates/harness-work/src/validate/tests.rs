use super::*;
use crate::create::create;
use crate::item::Kind;

#[test]
fn templates_are_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let cases = [
        (Kind::Deck, "pitch-deck"),
        (Kind::Book, "budget"),
        (Kind::Book, "kpi-tracker"),
        (Kind::Doc, "proposal"),
        (Kind::Doc, "weekly-report"),
        (Kind::Doc, "meeting-notes"),
    ];
    for (kind, id) in cases {
        let item = create(tmp.path(), kind, id, id).unwrap();
        let errors: Vec<Issue> = validate(&item)
            .into_iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{id}: {errors:?}");
    }
}

#[test]
fn reports_paths_for_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let item = create(tmp.path(), Kind::Deck, "Bad", "").unwrap();
    let deck = serde_json::json!({"format": "harness-deck/1", "slides": [
        {"id": "a", "layout": "bullets", "title": "x"},
        {"id": "b", "layout": "image", "title": "y", "image": "assets/none.png"}
    ]});
    std::fs::write(item.source_path(), deck.to_string()).unwrap();
    let issues = validate(&item);
    assert!(
        issues
            .iter()
            .any(|i| i.path == "deck.json#/slides/0/bullets"),
        "{issues:?}"
    );
    assert!(
        issues
            .iter()
            .any(|i| i.path.starts_with("deck.json#/slides/1/image")),
        "{issues:?}"
    );
    std::fs::write(item.source_path(), "{ not json").unwrap();
    let broken = validate(&item);
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert_eq!(broken[0].severity, Severity::Error);
}
