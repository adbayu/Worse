use std::io::Read as _;

use super::*;
use crate::create::create;

fn zip_parts(path: &std::path::Path) -> Vec<(String, String)> {
    let file = std::fs::File::open(path).unwrap();
    let mut z = zip::ZipArchive::new(file).unwrap();
    let mut out = Vec::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let name = f.name().unwrap().to_string();
        let mut s = String::new();
        if std::path::Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xml") || e.eq_ignore_ascii_case("rels"))
        {
            f.read_to_string(&mut s).unwrap();
        }
        out.push((name, s));
    }
    out
}

#[test]
fn pptx_parts_are_well_formed() {
    let tmp = tempfile::tempdir().unwrap();
    let item = create(tmp.path(), Kind::Deck, "Pitch", "pitch-deck").unwrap();
    let files = export(&item, ExportFormat::Pptx).unwrap();
    let parts = zip_parts(&files[0]);
    assert!(parts.iter().any(|(n, _)| n == "ppt/slides/slide8.xml"));
    assert!(parts.iter().any(|(n, _)| n.starts_with("ppt/charts/chart")));
    for (name, xml) in parts.iter().filter(|(_, x)| !x.is_empty()) {
        roxmltree::Document::parse(xml).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn docx_and_book_exports() {
    let tmp = tempfile::tempdir().unwrap();
    let doc = create(tmp.path(), Kind::Doc, "Proposal", "proposal").unwrap();
    let files = export(&doc, ExportFormat::Docx).unwrap();
    assert!(zip_parts(&files[0])
        .iter()
        .any(|(n, _)| n == "word/document.xml"));
    let html = export(&doc, ExportFormat::Html).unwrap();
    assert!(std::fs::read_to_string(&html[0]).unwrap().contains("<h1"));

    let book = create(tmp.path(), Kind::Book, "Budget", "budget").unwrap();
    let csv = export(&book, ExportFormat::Csv).unwrap();
    let text = std::fs::read_to_string(&csv[0]).unwrap();
    assert!(text.starts_with("Category,Planned"), "{text}");
    let xlsx = export(&book, ExportFormat::Xlsx).unwrap();
    assert!(xlsx[0].is_file());
    assert!(export(&book, ExportFormat::Pptx).is_err());
}

#[test]
fn parses_format_names() {
    assert_eq!(ExportFormat::parse("PowerPoint"), Some(ExportFormat::Pptx));
    assert_eq!(ExportFormat::parse(".xlsx"), Some(ExportFormat::Xlsx));
    assert_eq!(ExportFormat::parse("odt"), None);
}
