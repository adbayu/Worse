use super::parts::diff_counts;
use super::rows::Trail;
use super::ToolContent;

fn diff(old: Option<&str>, new: &str) -> ToolContent {
    ToolContent::Diff {
        path: "a.rs".into(),
        old_text: old.map(Into::into),
        new_text: new.into(),
    }
}

#[test]
fn diff_counts_only_the_changed_middle() {
    assert_eq!(
        diff_counts(&[diff(Some("a\nb\nc\nd"), "a\nx\ny\nd")]),
        (2, 2)
    );
    assert_eq!(diff_counts(&[diff(Some("a\nb"), "a\nb\nc")]), (1, 0));
    assert_eq!(diff_counts(&[diff(Some("a\nb\nc"), "a\nc")]), (0, 1));
    assert_eq!(diff_counts(&[diff(None, "one\ntwo\nthree")]), (3, 0));
    assert_eq!(diff_counts(&[diff(Some("same"), "same")]), (0, 0));
    assert_eq!(
        diff_counts(&[diff(None, "a\nb"), diff(Some("x"), "y")]),
        (3, 1)
    );
}

#[test]
fn trail_keeps_the_steps_before_the_current_one() {
    let mut trail = Trail::default();
    assert_eq!(trail.note("Read a.rs"), [] as [std::string::String; 0]);
    assert!(
        trail.note("Read a.rs").is_empty(),
        "the same step reported twice counts once"
    );
    assert_eq!(trail.note("Grep x"), vec!["Read a.rs"]);
    assert_eq!(trail.note("Edit a.rs"), vec!["Grep x", "Read a.rs"]);
    assert_eq!(trail.steps, 3);
    // Thinking (no activity): the last finished step leads.
    assert_eq!(trail.note(""), vec!["Edit a.rs", "Grep x", "Read a.rs"]);
    assert_eq!(
        trail.note("cargo test"),
        vec!["Edit a.rs", "Grep x", "Read a.rs"]
    );
    assert_eq!(
        trail.note("Read b.rs"),
        vec!["cargo test", "Edit a.rs", "Grep x"]
    );
    assert_eq!(trail.steps, 5);
}
