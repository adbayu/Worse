use super::*;
use crate::create::create;
use crate::export::{export, ExportFormat};
use crate::item::Source;

#[test]
fn pptx_round_trip_keeps_slides_and_text() {
    let tmp = tempfile::tempdir().unwrap();
    let item = create(tmp.path(), Kind::Deck, "Pitch", "pitch-deck").unwrap();
    let file = export(&item, ExportFormat::Pptx).unwrap().remove(0);
    let report = import(tmp.path(), &file).unwrap();
    assert_eq!(report.kind, Kind::Deck);
    let back = WorkItem::open(&report.dir).unwrap();
    let Source::Deck(d) = back.load().unwrap() else {
        panic!("not a deck")
    };
    assert_eq!(d.slides.len(), 8);
    assert_eq!(d.slides[0].layout, crate::model::deck::Layout::Title);
    assert!(
        d.slides[1]
            .bullets
            .iter()
            .any(|b| b.contains("current options")),
        "{:?}",
        d.slides[1]
    );
    assert!(d.slides.iter().any(|s| s.table.is_some()));
}

#[test]
fn docx_round_trip_keeps_headings_and_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let item = create(tmp.path(), Kind::Doc, "Notes", "meeting-notes").unwrap();
    let file = export(&item, ExportFormat::Docx).unwrap().remove(0);
    let report = import(tmp.path(), &file).unwrap();
    let text = std::fs::read_to_string(report.dir.join("doc.md")).unwrap();
    assert!(text.contains("## Decisions"), "{text}");
    assert!(text.contains("| Owner | Action | Due |"), "{text}");
}

#[test]
fn csv_becomes_a_book() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("sales.csv");
    std::fs::write(&file, "Region,Q1\nEMEA,12\n\"APAC, North\",9\n").unwrap();
    let report = import(tmp.path(), &file).unwrap();
    let Source::Book(b) = WorkItem::open(&report.dir).unwrap().load().unwrap() else {
        panic!("not a book")
    };
    assert_eq!(
        b.sheets[0].cells.get("A3").and_then(|v| v.as_str()),
        Some("APAC, North")
    );
    assert_eq!(
        b.sheets[0]
            .cells
            .get("B2")
            .and_then(serde_json::Value::as_f64),
        Some(12.0)
    );
    assert!(report.dir.join("assets/original/sales.csv").is_file());
}

#[test]
fn xlsx_round_trip_keeps_formulas() {
    let tmp = tempfile::tempdir().unwrap();
    let item = create(tmp.path(), Kind::Book, "Budget", "budget").unwrap();
    let file = export(&item, ExportFormat::Xlsx).unwrap().remove(0);
    let report = import(tmp.path(), &file).unwrap();
    let Source::Book(b) = WorkItem::open(&report.dir).unwrap().load().unwrap() else {
        panic!("not a book")
    };
    assert_eq!(
        b.sheets[0].cells.get("B7").and_then(|v| v.as_str()),
        Some("=SUM(B2:B6)")
    );
}
