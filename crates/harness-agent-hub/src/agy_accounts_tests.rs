#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;

use super::*;
use crate::agy_cred::fake::MemStore;
use crate::agy_cred::CredBlob;

fn blob(secret: &[u8]) -> CredBlob {
    let mut out = CredBlob::default();
    out.persist = 2;
    out.user_name = Some("antigravity".into());
    out.secret = secret.to_vec();
    out
}

fn login(store: &MemStore, secret: &[u8]) {
    store.write(AGY_LIVE_TARGET, &blob(secret)).unwrap();
}

#[test]
fn json_round_trip() {
    let mut file = AgyFile::default();
    let a = file.add("Work", 1).unwrap();
    let b = file.add("Work", 2).unwrap();
    assert_eq!((a.as_str(), b.as_str()), ("work", "work-2"));
    file.active = Some(b.clone());
    let text = serde_json::to_string(&file).unwrap();
    assert!(text.contains("\"created_ms\":1"));
    assert!(!text.contains("pending"));
    let back: AgyFile = serde_json::from_str(&text).unwrap();
    assert_eq!(back, file);
    let empty: AgyFile = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, AgyFile::default());
}

#[test]
fn normalize_order_and_names() {
    let mut file = AgyFile::default();
    assert!(file.add("  ", 0).is_err());
    let a = file.add("A", 0).unwrap();
    let b = file.add("B", 0).unwrap();
    file.set_order(vec![b.clone(), "ghost".into(), b.clone()]);
    assert_eq!(file.order, vec![b.clone(), a.clone()]);
}

#[test]
fn import_sign_in_use_and_remove() {
    let store = MemStore::default();
    let mut file = AgyFile::default();
    assert!(import_live(&store, &mut file, "Main", 0).is_err());
    login(&store, b"one");
    let main = import_live(&store, &mut file, "Main", 0).unwrap();
    assert_eq!(file.active.as_deref(), Some(main.as_str()));
    assert_eq!(store.secret(&slot_target(&main)).unwrap(), b"one");

    let second = file.add("Second", 0).unwrap();
    assert!(use_account(&store, &mut file, &second).is_err());
    login(&store, b"one-refreshed");
    begin_sign_in(&store, &mut file, &second).unwrap();
    assert_eq!(store.secret(&slot_target(&main)).unwrap(), b"one-refreshed");
    assert!(store.secret(AGY_LIVE_TARGET).is_none());
    assert_eq!(file.active, None);
    assert_eq!(finish_sign_in(&store, &mut file).unwrap(), Finish::Waiting);
    login(&store, b"two");
    assert_eq!(finish_sign_in(&store, &mut file).unwrap(), Finish::Done);
    assert_eq!(file.active.as_deref(), Some(second.as_str()));
    assert_eq!(file.pending, None);

    use_account(&store, &mut file, &main).unwrap();
    assert_eq!(store.secret(AGY_LIVE_TARGET).unwrap(), b"one-refreshed");
    assert_eq!(store.secret(&slot_target(&second)).unwrap(), b"two");

    remove(&store, &mut file, &main).unwrap();
    assert_eq!(file.active, None);
    assert!(store.secret(&slot_target(&main)).is_none());
    assert!(store.secret(AGY_LIVE_TARGET).is_none());
    assert_eq!(file.order, vec![second]);
}

#[test]
fn fallback_skips_active_exhausted_and_signed_out() {
    let store = MemStore::default();
    let mut file = AgyFile::default();
    let a = file.add("A", 0).unwrap();
    let b = file.add("B", 0).unwrap();
    let c = file.add("C", 0).unwrap();
    let d = file.add("D", 0).unwrap();
    for id in [&a, &c, &d] {
        store.write(&slot_target(id), &blob(id.as_bytes())).unwrap();
    }
    file.active = Some(a.clone());
    let has = |id: &str| store.exists(&slot_target(id)).unwrap();
    let mut exhausted = HashSet::from([a.clone()]);
    assert_eq!(file.next_fallback(&has, &exhausted), Some(c.clone()));
    exhausted.insert(c.clone());
    assert_eq!(file.next_fallback(&has, &exhausted), Some(d.clone()));
    exhausted.insert(d);
    assert_eq!(file.next_fallback(&has, &exhausted), None);
    file.active = None;
    assert_eq!(file.next_fallback(&has, &HashSet::new()), Some(a));
    let _ = b;
}

#[test]
fn begin_sign_in_auto_imports_live_when_inactive() {
    let store = MemStore::default();
    let mut file = AgyFile::default();
    login(&store, b"pre-existing");
    let work = file.add("Work", 0).unwrap();
    begin_sign_in(&store, &mut file, &work).unwrap();
    assert!(store.secret(AGY_LIVE_TARGET).is_none());
    assert_eq!(store.secret(&slot_target("main")).unwrap(), b"pre-existing");
    assert_eq!(file.accounts.len(), 2);
    assert_eq!(file.accounts[1].name, "Main");
    assert_eq!(file.pending.as_deref(), Some(work.as_str()));

    // A second unimported live login when "Main" is already taken gets "Main 2"; an identical slot is not duplicated.
    login(&store, b"pre-existing");
    file.active = None;
    file.pending = None;
    begin_sign_in(&store, &mut file, &work).unwrap();
    assert_eq!(file.accounts.len(), 2);

    login(&store, b"another-unimported");
    file.pending = None;
    begin_sign_in(&store, &mut file, &work).unwrap();
    assert_eq!(file.accounts.len(), 3);
    assert_eq!(file.accounts[2].name, "Main 2");
    assert_eq!(
        store.secret(&slot_target("main-2")).unwrap(),
        b"another-unimported"
    );
}

#[test]
fn view_and_debug_never_show_secrets() {
    let store = MemStore::default();
    let mut file = AgyFile::default();
    login(&store, b"top-secret");
    let id = import_live(&store, &mut file, "Main", 5).unwrap();
    let shown = view(&store, &file, &HashSet::new());
    assert!(shown.available && shown.live_signed_in);
    assert!(shown.accounts[0].signed_in && shown.accounts[0].active);
    let debug = format!("{:?}", store.read(&slot_target(&id)).unwrap());
    assert!(!debug.contains("top-secret"));
    let unavailable = crate::agy_cred::UnavailableCredStore;
    assert!(!view(&unavailable, &file, &HashSet::new()).available);
}

#[test]
fn live_login_during_pending_sign_in_goes_to_pending_account() {
    let store = MemStore::default();
    let mut file = AgyFile::default();
    login(&store, b"main-token");
    let work = file.add("Work", 0).unwrap();
    begin_sign_in(&store, &mut file, &work).unwrap();
    assert_eq!(file.accounts.len(), 2);

    login(&store, b"work-token");
    let other = file.add("Other", 0).unwrap();
    begin_sign_in(&store, &mut file, &other).unwrap();
    assert_eq!(file.accounts.len(), 3);
    assert_eq!(store.secret(&slot_target(&work)).unwrap(), b"work-token");
    assert_eq!(file.pending.as_deref(), Some(other.as_str()));
}

#[test]
fn refreshed_token_of_auto_imported_login_is_not_duplicated() {
    let store = MemStore::default();
    let mut file = AgyFile::default();
    login(&store, b"token-v1");
    save_live(&store, &mut file).unwrap();
    assert_eq!(file.active.as_deref(), Some("main"));

    login(&store, b"token-v2-refreshed");
    let sek = file.add("Sek2", 0).unwrap();
    begin_sign_in(&store, &mut file, &sek).unwrap();
    assert_eq!(file.accounts.len(), 2);
    assert_eq!(
        store.secret(&slot_target("main")).unwrap(),
        b"token-v2-refreshed"
    );
}

#[test]
fn same_email_with_new_token_is_not_imported_again() {
    let store = MemStore::default();
    let mut file = AgyFile::default();
    login(
        &store,
        br#"{"token":{"access_token":"a1"},"id_token_email":"me@example.com"}"#,
    );
    save_live(&store, &mut file).unwrap();
    file.active = None;
    login(
        &store,
        br#"{"token":{"access_token":"a2"},"id_token_email":"Me@Example.com"}"#,
    );
    let sek = file.add("Sek2", 0).unwrap();
    begin_sign_in(&store, &mut file, &sek).unwrap();
    assert_eq!(file.accounts.len(), 2);
    assert!(file.accounts.iter().all(|a| a.name != "Main 2"));
}

fn google(email: &str, token: &str) -> Vec<u8> {
    format!(r#"{{"token":{{"access_token":"{token}"}},"id_token_email":"{email}"}}"#).into_bytes()
}

#[test]
fn signing_in_with_the_same_google_account_does_not_clone_it() {
    let store = MemStore::default();
    let mut file = AgyFile::default();
    login(&store, &google("main@example.com", "m1"));
    let main = import_live(&store, &mut file, "Main", 0).unwrap();
    let sek = file.add("Sek2", 0).unwrap();
    begin_sign_in(&store, &mut file, &sek).unwrap();
    login(&store, &google("main@example.com", "m2"));
    assert_eq!(
        finish_sign_in(&store, &mut file).unwrap(),
        Finish::SameAs("Main".into())
    );
    assert!(store.secret(&slot_target(&sek)).is_none());
    assert_eq!(file.active.as_deref(), Some(main.as_str()));
    assert_eq!(file.pending, None);
    assert_eq!(
        store.secret(&slot_target(&main)).unwrap(),
        google("main@example.com", "m2")
    );

    begin_sign_in(&store, &mut file, &sek).unwrap();
    login(&store, &google("sek2@example.com", "s1"));
    assert_eq!(finish_sign_in(&store, &mut file).unwrap(), Finish::Done);
    assert_eq!(
        store.secret(&slot_target(&sek)).unwrap(),
        google("sek2@example.com", "s1")
    );
}

#[test]
fn switching_while_a_sign_in_waits_does_not_store_the_old_login_in_it() {
    let store = MemStore::default();
    let mut file = AgyFile::default();
    login(&store, &google("main@example.com", "m1"));
    let main = import_live(&store, &mut file, "Main", 0).unwrap();
    let sek = file.add("Sek2", 0).unwrap();
    begin_sign_in(&store, &mut file, &sek).unwrap();
    login(&store, &google("main@example.com", "m2"));
    use_account(&store, &mut file, &main).unwrap();
    assert!(store.secret(&slot_target(&sek)).is_none());
    assert_eq!(file.pending, None);
    assert_eq!(file.accounts.len(), 2);
}

#[test]
fn removing_the_active_account_does_not_bring_it_back_as_main() {
    let store = MemStore::default();
    let mut file = AgyFile::default();
    login(&store, &google("old@example.com", "o1"));
    let old = import_live(&store, &mut file, "Old", 0).unwrap();
    let sek = file.add("Sek2", 0).unwrap();
    begin_sign_in(&store, &mut file, &sek).unwrap();
    login(&store, &google("sek2@example.com", "s1"));
    assert_eq!(finish_sign_in(&store, &mut file).unwrap(), Finish::Done);
    use_account(&store, &mut file, &old).unwrap();
    remove(&store, &mut file, &old).unwrap();
    use_account(&store, &mut file, &sek).unwrap();
    assert_eq!(file.accounts.len(), 1);
    assert!(file.accounts.iter().all(|a| !a.name.starts_with("Main")));
}
