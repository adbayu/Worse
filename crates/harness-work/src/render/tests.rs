use super::*;
use crate::create::create;
use crate::item::Kind;

#[test]
fn deck_renders_pages_and_hits() {
    let tmp = tempfile::tempdir().unwrap();
    let item = create(tmp.path(), Kind::Deck, "Pitch", "pitch-deck").unwrap();
    let out = render(&item, &Target::All, 0.25).unwrap();
    assert_eq!(out.page_count, 8);
    assert_eq!(out.pages.len(), 8);
    assert!(out.pages[0].png.starts_with(b"\x89PNG"));
    assert!(
        out.hits
            .iter()
            .any(|h| h.path.starts_with("deck.json#/slides/1/")),
        "{:?}",
        out.hits
    );
    let one = render(&item, &Target::Page(2), 0.25).unwrap();
    assert_eq!(one.pages.len(), 1);
    assert_eq!(one.pages[0].index, 2);
}

#[test]
fn doc_book_chart_render_and_pdf() {
    let tmp = tempfile::tempdir().unwrap();
    let doc = create(tmp.path(), Kind::Doc, "Proposal", "proposal").unwrap();
    let out = render(&doc, &Target::All, 0.25).unwrap();
    // 修复：页数取决于系统字体（CI 的 Windows 机器上只有 1 页），只检查确实渲染出了页面且页数一致。
    assert_ne!(out.pages.len(), 0);
    assert_eq!(out.pages.len(), out.page_count);
    assert!(render_pdf(&doc).unwrap().starts_with(b"%PDF"));
    let book = create(tmp.path(), Kind::Book, "Budget", "budget").unwrap();
    assert_ne!(
        render(&book, &Target::parse("Budget!A1:E7"), 0.25)
            .unwrap()
            .pages
            .len(),
        0
    );
    let chart = create(tmp.path(), Kind::Chart, "C", "").unwrap();
    assert_eq!(render(&chart, &Target::All, 0.25).unwrap().pages.len(), 1);
}

#[test]
fn parses_targets() {
    assert_eq!(Target::parse("slide 3"), Target::Page(2));
    assert_eq!(Target::parse(""), Target::All);
    assert_eq!(
        Target::parse("'My Sheet'!A1:B2"),
        Target::Range {
            sheet: "My Sheet".into(),
            range: "A1:B2".into()
        }
    );
}
