use super::*;
use crate::item::Source;

#[test]
fn creates_unique_folders_with_titles() {
    let tmp = tempfile::tempdir().unwrap();
    let a = create(tmp.path(), Kind::Deck, "Q3 Review!", "").unwrap();
    let b = create(tmp.path(), Kind::Deck, "Q3 Review!", "").unwrap();
    assert_eq!(a.slug(), "q3-review");
    assert_ne!(a.dir, b.dir);
    let Source::Deck(d) = a.load().unwrap() else {
        panic!("not a deck")
    };
    assert_eq!(d.title, "Q3 Review!");
    assert!(create(tmp.path(), Kind::Deck, "x", "budget").is_err());
}

#[test]
fn templates_list_and_save() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(templates(tmp.path()).iter().any(|t| t.id == "pitch-deck"));
    let item = create(tmp.path(), Kind::Deck, "Ours", "pitch-deck").unwrap();
    save_template(tmp.path(), &item, "Our pitch").unwrap();
    let list = templates(tmp.path());
    let ours = list
        .iter()
        .find(|t| t.origin == "project")
        .expect("project template");
    let copy = create(tmp.path(), Kind::Deck, "From ours", &ours.id).unwrap();
    assert!(copy.source_path().is_file());
}

#[test]
fn slugify_is_ascii() {
    assert_eq!(slugify("Hello, World"), "hello-world");
    assert_ne!(slugify(""), "");
}
