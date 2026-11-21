use super::*;

fn commit(hash: &str, parents: &[&str]) -> Commit {
    Commit {
        hash: hash.into(),
        parents: parents.iter().map(|p| (*p).to_owned()).collect(),
        author: "a".into(),
        time: 0,
        subject: hash.into(),
        refs: Vec::new(),
    }
}

#[test]
fn merge_and_branch_lanes() {
    // m merges b into a; both come from root.
    let rows = lanes(vec![
        commit("m", &["a", "b"]),
        commit("b", &["r"]),
        commit("a", &["r"]),
        commit("r", &[]),
    ]);
    let at = index(&rows);
    let row = |h: &str| &rows[at[h]];
    assert_eq!(row("m").lane, 0);
    assert_eq!(row("m").parent_lanes, vec![0, 1]);
    assert_eq!(row("b").lane, 1);
    assert_eq!(row("a").lane, 0);
    assert_eq!(row("r").lane, 0);
    assert_eq!(row("b").lanes, 2);
    assert_eq!(row("r").lanes, 1);
}

#[test]
fn parses_records_and_refs() {
    let text = "h1\u{1f}p1 p2\u{1f}Ann\u{1f}42\u{1f}subj\u{1f}HEAD -> main, origin/main\u{1e}\nh2\u{1f}\u{1f}Bob\u{1f}7\u{1f}root\u{1f}\u{1e}\n";
    let commits = parse_log(text);
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].parents, vec!["p1", "p2"]);
    assert_eq!(commits[0].refs, vec!["HEAD -> main", "origin/main"]);
    assert_eq!(commits[1].parents, Vec::<String>::new());
    assert_eq!(commits[1].time, 7);
}

#[test]
fn blocked_switch_files() {
    let stderr = "error: Your local changes to the following files would be overwritten by checkout:\n\ta.txt\n\tdir/b.txt\nPlease commit your changes or stash them before you switch branches.\nAborting";
    assert_eq!(
        overwritten_files(stderr),
        Some(vec!["a.txt".to_owned(), "dir/b.txt".to_owned()])
    );
    assert_eq!(overwritten_files("fatal: invalid reference: x"), None);
}
