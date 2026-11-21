use super::*;

fn memory(id: &str, at: i64, kind: Kind, text: &str) -> Memory {
    Memory {
        id: id.into(),
        at,
        kind,
        text: text.into(),
        project: Some("/p".into()),
        pinned: false,
        session: None,
        agent: None,
        title: None,
        key: None,
        replaces: None,
        expires: None,
    }
}

#[test]
fn words_split_identifiers_and_drop_filler() {
    assert_eq!(
        tokens("TokenPairing.is_live()"),
        ["token", "pairing", "tokenpairing", "live"]
    );
    assert_eq!(
        tokens("fix the HTTPServer tokens"),
        ["fix", "http", "server", "httpserver", "token"]
    );
    assert_eq!(
        tokens("gimana biar bisa build di Windows"),
        ["build", "window"]
    );
    assert!(tokens("the and of").is_empty());
}

#[test]
fn search_ranks_by_words_then_recency() {
    let all = vec![
        memory(
            "a",
            1_000,
            Kind::Fact,
            "The pairing token expires after ten minutes",
        ),
        memory(
            "b",
            2_000,
            Kind::Decision,
            "Use GitHub Actions for the Windows build",
        ),
        memory(
            "c",
            3_000,
            Kind::Fact,
            "TokenPairing reads the clock once per check",
        ),
        memory(
            "d",
            4_000,
            Kind::Fact,
            "The wallpaper is blurred on a worker thread",
        ),
    ];
    let refs: Vec<&Memory> = all.iter().collect();
    let ids = |text: &str, need: usize| -> Vec<&str> {
        search(
            &refs,
            &Query {
                text,
                limit: 5,
                need,
                kind: None,
            },
        )
        .iter()
        .map(|h| h.memory.id.as_str())
        .collect()
    };
    // Both pairing memories match; the one with both words comes first.
    assert_eq!(ids("pairing token expiry", 1), ["a", "c"]);
    assert_eq!(ids("windows build", 1), ["b"]);
    assert!(ids("database migration", 1).is_empty());
    // Two words needed: "pairing" alone is not enough for c… but c has token+pairing through its identifier.
    assert_eq!(ids("pairing clock", 2), ["c"]);
    // Same words, different age: the newer one wins the tie.
    let twins = vec![
        memory("old", 1_000, Kind::Fact, "deploy uses the staging cluster"),
        memory("new", 9_000, Kind::Fact, "deploy uses the staging cluster"),
    ];
    let refs: Vec<&Memory> = twins.iter().collect();
    let hits = search(
        &refs,
        &Query {
            text: "staging deploy",
            limit: 5,
            need: 1,
            kind: None,
        },
    );
    assert_eq!(hits[0].memory.id, "new");
    let only = search(
        &refs,
        &Query {
            text: "staging",
            limit: 5,
            need: 1,
            kind: Some(Kind::Decision),
        },
    );
    assert!(only.is_empty());
}

#[test]
fn recall_block_has_pinned_related_and_earlier_sessions() {
    let now = 10 * 24 * 60 * 60 * 1000;
    let hour = 60 * 60 * 1000;
    let mut pinned = memory(
        "pin",
        now - 5 * hour,
        Kind::Preference,
        "Reply in Indonesian, casual",
    );
    pinned.pinned = true;
    pinned.project = None;
    let mut related = memory(
        "rel",
        now - 3 * 24 * hour,
        Kind::Decision,
        "Pairing tokens expire after ten minutes because of replay attacks",
    );
    related.agent = Some("Claude Code".into());
    let unrelated = memory(
        "unr",
        now - hour,
        Kind::Fact,
        "The wallpaper is blurred on a worker thread",
    );
    let mut episode = memory(
        "epi",
        now - 2 * hour,
        Kind::Episode,
        "Asked: add the Windows build | Result: zip uploaded",
    );
    episode.session = Some("s-old".into());
    episode.agent = Some("Codex".into());
    episode.title = Some("Windows build".into());
    let mut own = memory(
        "own",
        now - hour,
        Kind::Episode,
        "Asked: pairing token expiry",
    );
    own.session = Some("s-now".into());
    let mut elsewhere = memory(
        "els",
        now - hour,
        Kind::Fact,
        "pairing token note of another project",
    );
    elsewhere.project = Some("/other".into());
    let all = vec![pinned, related, unrelated, episode, own, elsewhere];
    let block = recall_block(&RecallInput {
        memories: &all,
        project: Some("/p"),
        session: "s-now",
        query: "why does the pairing token expire so fast?",
        now,
    });
    assert!(block.starts_with("<harness-recall>"), "{block}");
    assert!(block.contains("Always:\n- [pin] preference: Reply in Indonesian"));
    assert!(block.contains(
        "Related to this request:\n- [rel] decision · 3d ago · Claude Code: Pairing tokens"
    ));
    assert!(block.contains(
        "Earlier sessions in this project:\n- [epi] 2h ago · Codex · \"Windows build\": Asked"
    ));
    for hidden in ["[unr]", "[own]", "[els]"] {
        assert!(!block.contains(hidden), "{hidden} in {block}");
    }
    assert!(block.ends_with(INSTRUCTIONS));
    // Nothing stored: only the instructions.
    let empty = recall_block(&RecallInput {
        memories: &[],
        project: Some("/p"),
        session: "s",
        query: "hi",
        now,
    });
    assert_eq!(empty, INSTRUCTIONS);
}

#[test]
fn recall_block_stays_inside_its_budget() {
    let now = 1_000_000_000;
    let all: Vec<Memory> = (0..60)
        .map(|i| {
            let mut m = memory(
                &format!("m{i}"),
                now - i64::from(i) * 1000,
                Kind::Fact,
                &format!("pairing token detail number {i} {}", "word ".repeat(80)),
            );
            m.pinned = i % 2 == 0;
            m
        })
        .collect();
    let block = recall_block(&RecallInput {
        memories: &all,
        project: Some("/p"),
        session: "s",
        query: "pairing token",
        now,
    });
    assert!(
        block.chars().count() < 3_200 + INSTRUCTIONS.len() + 400,
        "{}",
        block.chars().count()
    );
    assert!(block.contains("Always:") && block.contains("Related to this request:"));
}

#[test]
fn episode_text_keeps_first_ask_last_asks_result_tail_and_files() {
    let turns = vec![
        EpisodeTurn { user: "fix the pairing expiry bug".into(), assistant: "Looking.".into(), files: vec!["src/auth.rs".into()] },
        EpisodeTurn { user: "also add a test".into(), assistant: format!("{} The check now takes the time from the caller and a frozen-clock test covers it.", "preamble ".repeat(80)), files: vec!["src/auth.rs".into(), "src/tests.rs".into()] },
    ];
    let text = episode_text(&turns);
    assert!(
        text.starts_with("Asked: fix the pairing expiry bug | Then: also add a test | Result: …"),
        "{text}"
    );
    assert!(text.contains("frozen-clock test covers it. | Files: src/auth.rs, src/tests.rs"));
    assert!(episode_text(&[]).is_empty());
    assert_eq!(age(10 * 60_000, 0), "10m ago");
    assert_eq!(age(26 * 60 * 60_000, 0), "1d ago");
}

#[test]
fn project_keys_ignore_separators_and_trailing_slash() {
    assert_eq!(project_key("/home/u/proj/"), "/home/u/proj");
    assert_eq!(
        project_key("C:\\ws\\"),
        if cfg!(windows) { "c:/ws" } else { "C:/ws" }
    );
}
