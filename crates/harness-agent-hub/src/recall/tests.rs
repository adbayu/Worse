#![allow(clippy::useless_vec, clippy::assert_is_empty)]
use super::*;

mod search;

fn store() -> (Store, PathBuf) {
    let home =
        std::env::temp_dir().join(format!("harness-recall-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&home).unwrap();
    (Store::new(&home), home)
}

fn fact(text: &str, project: Option<&str>) -> NewMemory {
    NewMemory {
        text: text.into(),
        project: project.map(str::to_owned),
        ..NewMemory::default()
    }
}

#[test]
fn add_reload_and_scopes() {
    let (store, home) = store();
    let a = store
        .add(fact("The API server listens on port 8642", Some("/p/one")))
        .unwrap();
    store.add(fact("Reply in Indonesian", None)).unwrap();
    store
        .add(fact("Other project fact", Some("/p/two")))
        .unwrap();
    let all = Store::new(&home).load();
    assert_eq!(all.len(), 3);
    assert_eq!(visible(&all, Some("/p/one")).len(), 2);
    assert_eq!(visible(&all, None).len(), 1);
    assert!(matches!(a, Added::New(_)));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn the_same_text_is_stored_once_per_scope() {
    let (store, home) = store();
    let first = store
        .add(fact("Tests run with  cargo test", Some("/p")))
        .unwrap();
    let again = store
        .add(fact("tests run with cargo test", Some("/p")))
        .unwrap();
    assert_eq!(again, Added::Duplicate(first.id().to_owned()));
    assert!(matches!(
        store.add(fact("tests run with cargo test", None)).unwrap(),
        Added::New(_)
    ));
    assert_eq!(store.load().len(), 2);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn replace_supersedes_and_forget_removes() {
    let (store, home) = store();
    let old = store.add(fact("Deploys go through Jenkins", None)).unwrap();
    let new = store
        .add(NewMemory {
            replaces: Some(old.id().to_owned()),
            ..fact("Deploys go through GitHub Actions", None)
        })
        .unwrap();
    let live = store.load();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, new.id());
    assert!(store
        .add(NewMemory {
            replaces: Some("nope".into()),
            ..fact("x y z", None)
        })
        .is_err());
    assert_eq!(store.forget(new.id()).unwrap(), 1);
    assert!(store.load().is_empty());
    // By exact text when no id matches.
    store.add(fact("Remember the milk", None)).unwrap();
    assert_eq!(store.forget("remember the  MILK").unwrap(), 1);
    assert_eq!(store.forget("nothing like it").unwrap(), 0);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn preferences_are_pinned_and_pins_can_change() {
    let (store, home) = store();
    let pref = store
        .add(NewMemory {
            kind: Some(Kind::Preference),
            ..fact("Short answers", None)
        })
        .unwrap();
    let plain = store.add(fact("Port 8642", None)).unwrap();
    assert!(
        store
            .load()
            .iter()
            .find(|m| m.id == pref.id())
            .unwrap()
            .pinned
    );
    assert!(store.set_pinned(plain.id(), true).unwrap());
    assert!(store.load().iter().all(|m| m.pinned));
    assert!(store.set_pinned(pref.id(), false).unwrap());
    assert!(
        !store
            .load()
            .iter()
            .find(|m| m.id == pref.id())
            .unwrap()
            .pinned
    );
    assert!(!store.set_pinned("unknown", true).unwrap());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn credentials_are_refused_and_redacted() {
    let (store, home) = store();
    for text in [
        "the key is sk-abcdefghijklmnopqrstuvwxyz012345",
        "password = hunter2secret9",
        "\"api_key\": \"a1b2c3d4e5f6\"",
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----",
        "looks fine\u{200B}but hides text",
    ] {
        assert!(store.add(fact(text, None)).is_err(), "{text}");
    }
    // Words that only mention a credential are fine.
    for text in [
        "The token is read from the COMMAND_CODE_API_KEY variable",
        "password: required field in the login form",
        "commit 3f2a9c1e5b7d8a6f4c2e1b0a9d8c7b6a5f4e3d2c fixed it",
    ] {
        assert!(store.add(fact(text, None)).is_ok(), "{text}");
    }
    let cleaned = redact("use token=abc12345xyz then call sk-abcdefghijklmnopqrstuvwxyz with care");
    assert!(
        !cleaned.contains("abc12345xyz") && !cleaned.contains("sk-abc"),
        "{cleaned}"
    );
    assert!(cleaned.contains("[redacted]") && cleaned.ends_with("with care"));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn an_episode_takes_the_place_of_the_sessions_earlier_one() {
    let (store, home) = store();
    store
        .put_episode("s1", "/p/", "Fix pairing", "Codex", "Asked: fix it")
        .unwrap();
    store
        .put_episode(
            "s1",
            "/p",
            "Fix pairing",
            "Claude Code",
            "Asked: fix it | Result: done",
        )
        .unwrap();
    store
        .put_episode(
            "s2",
            "/p",
            "Other",
            "Codex",
            "Asked: other, token=abc12345xyz",
        )
        .unwrap();
    let live = store.load();
    assert_eq!(live.len(), 2);
    let first = live
        .iter()
        .find(|m| m.session.as_deref() == Some("s1"))
        .unwrap();
    assert_eq!(first.agent.as_deref(), Some("Claude Code"));
    assert_eq!(first.project.as_deref(), Some("/p"));
    assert!(first.text.ends_with("done"));
    assert!(!live.iter().any(|m| m.text.contains("abc12345xyz")));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_torn_last_line_is_skipped_and_the_next_write_survives() {
    let (store, home) = store();
    store.add(fact("first fact", None)).unwrap();
    let mut file = OpenOptions::new()
        .append(true)
        .open(store.log_path())
        .unwrap();
    file.write_all(b"{\"op\":\"put\",\"id\":\"m1\",\"te")
        .unwrap();
    drop(file);
    assert_eq!(store.load().len(), 1);
    store.add(fact("second fact", None)).unwrap();
    assert_eq!(store.load().len(), 2);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn dead_records_are_compacted_away() {
    let (store, home) = store();
    let keep = store.add(fact("kept forever", None)).unwrap();
    for i in 0..80 {
        store
            .put_episode("same-session", "/p", "t", "a", &format!("round {i}"))
            .unwrap();
    }
    let lines = fs::read_to_string(store.log_path())
        .unwrap()
        .lines()
        .count();
    assert!(lines < 70, "log has {lines} lines after 81 writes");
    let live = store.load();
    assert_eq!(live.len(), 2);
    assert!(live.iter().any(|m| m.id == keep.id()));
    assert!(live.iter().any(|m| m.text == "round 79"));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn concurrent_writers_lose_nothing() {
    let (store, home) = store();
    let mut threads = Vec::new();
    for t in 0..6 {
        let store = store.clone();
        threads.push(std::thread::spawn(move || {
            for i in 0..10 {
                store
                    .add(fact(&format!("writer {t} fact number {i}"), None))
                    .unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(store.load().len(), 60);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn expired_memories_are_not_live() {
    let (store, home) = store();
    store
        .add(NewMemory {
            ttl_days: Some(1),
            ..fact("temporary", None)
        })
        .unwrap();
    assert_eq!(store.load().len(), 1);
    assert!(store.fold(now_ms() + 2 * 24 * 60 * 60 * 1000).0.is_empty());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn config_defaults_on_and_round_trips() {
    let (store, home) = store();
    assert_eq!(store.config(), Config::default());
    assert!(store.config().recall && store.config().capture && store.config().graph);
    store
        .set_config(Config {
            recall: false,
            ..Config::default()
        })
        .unwrap();
    assert_eq!(
        store.config(),
        Config {
            recall: false,
            ..Config::default()
        }
    );
    // A file written before the graph switch existed still reads, with the switch on.
    fs::write(
        home.join("memory").join("config.json"),
        "{\"recall\":true,\"capture\":false}",
    )
    .unwrap();
    assert_eq!(
        store.config(),
        Config {
            recall: true,
            capture: false,
            graph: true,
            concise: true
        }
    );
    let _ = fs::remove_dir_all(home);
}
