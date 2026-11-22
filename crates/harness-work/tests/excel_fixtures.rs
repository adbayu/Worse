//! Real-world Excel fixture tests for Phases 1–5 (`docs/plans/excel-universal.md`):
//! - Loadsheet, budget, invoice, date schedule, lookup table (VLOOKUP/XLOOKUP/INDEX-MATCH)
//! - Defined names (workbook + sheet scope) and Excel Table structured-ref rewrite
//! - Lean `style.json` split + style coalescing
//! - Write-back `.xlsx` export preserving charts/macros/styles
//! - Locale formatting (`id-ID` decimal comma) and volatile/dynamic functions

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::io::{Cursor, Read as _, Write as _};

use harness_work::{
    export, export_clean, import, Book, CellValue, ExportFormat, Sheet, SheetModel, Source,
    WorkItem,
};
use serde_json::json;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

fn make_sheet(name: &str, entries: &[(&str, serde_json::Value)]) -> Sheet {
    let mut cells = BTreeMap::new();
    for (k, v) in entries {
        cells.insert((*k).to_string(), v.clone());
    }
    Sheet {
        name: name.to_string(),
        cells,
        ..Sheet::default()
    }
}

#[test]
fn real_world_formulas_and_named_ranges() {
    // Budget + Invoice + Lookup + Date schedule + Defined names
    let lookup = make_sheet(
        "Rates",
        &[
            ("A1", json!("Code")),
            ("B1", json!("Rate")),
            ("A2", json!("STD")),
            ("B2", json!(120)),
            ("A3", json!("PRO")),
            ("B3", json!(250)),
            ("A4", json!("ENT")),
            ("B4", json!(500)),
            ("C1", json!("TaxRate")),
            ("C2", json!(0.11)),
        ],
    );
    let invoice = make_sheet(
        "Invoice",
        &[
            ("A1", json!("Item")),
            ("B1", json!("Plan")),
            ("C1", json!("Qty")),
            ("D1", json!("Unit")),
            ("E1", json!("Total")),
            ("A2", json!("Seat")),
            ("B2", json!("PRO")),
            ("C2", json!(10)),
            ("D2", json!("=VLOOKUP(B2,Rates!$A$2:$B$4,2,FALSE)")),
            ("E2", json!("=C2*D2")),
            ("A3", json!("Support")),
            ("B3", json!("ENT")),
            ("C3", json!(2)),
            ("D3", json!("=XLOOKUP(B3,Rates!$A$2:$A$4,Rates!$B$2:$B$4)")),
            ("E3", json!("=C3*D3")),
            ("A4", json!("Subtotal")),
            ("E4", json!("=SUM(E2:E3)")),
            ("A5", json!("Tax")),
            ("E5", json!("=ROUND(E4*TaxRate,2)")),
            ("A6", json!("GrandTotal")),
            ("E6", json!("=E4+E5")),
            ("A7", json!("StartDate")),
            ("B7", json!("=DATE(2026,1,15)")),
            ("A8", json!("EndOfMonth")),
            ("B8", json!("=EDATE(B7,1)")),
            ("A9", json!("IfCheck")),
            ("B9", json!("=IF(E6>3000,\"LARGE\",\"SMALL\")")),
            ("A10", json!("SumIfs")),
            ("B10", json!("=SUMIFS(E2:E3,B2:B3,\"PRO\")")),
            ("A11", json!("IndexMatch")),
            (
                "B11",
                json!("=INDEX(Rates!B2:B4,MATCH(\"STD\",Rates!A2:A4,0))"),
            ),
        ],
    );
    let mut names = BTreeMap::new();
    names.insert("TaxRate".into(), "Rates!$C$2".into());
    let book = Book {
        title: "Invoice Fixture".into(),
        sheets: vec![invoice, lookup],
        names,
        ..Book::default()
    };

    let model = SheetModel::from_book(&book).unwrap();
    assert_eq!(model.value(0, 2, 4), CellValue::Number(250.0));
    assert_eq!(model.value(0, 3, 4), CellValue::Number(500.0));
    assert_eq!(model.value(0, 4, 5), CellValue::Number(3500.0));
    assert_eq!(model.value(0, 5, 5), CellValue::Number(385.0));
    assert_eq!(model.value(0, 6, 5), CellValue::Number(3885.0));
    assert_eq!(model.value(0, 9, 2), CellValue::String("LARGE".into()));
    assert_eq!(model.value(0, 10, 2), CellValue::Number(2500.0));
    assert_eq!(model.value(0, 11, 2), CellValue::Number(120.0));
}

#[test]
fn locale_id_formats_decimal_comma() {
    let mut sheet = make_sheet("Data", &[("A1", json!(12345.67))]);
    sheet.formats.insert("A1".into(), "#,##0.00".into());
    let book = Book {
        title: "ID Locale".into(),
        sheets: vec![sheet],
        locale: "id-ID".into(),
        ..Book::default()
    };
    let model = SheetModel::from_book(&book).unwrap();
    assert_eq!(model.formatted(0, 1, 1), "12.345,67");
}

#[test]
fn write_back_xlsx_preserves_extra_parts_and_updates_cells() {
    let tmp = tempfile::tempdir().unwrap();
    // 1. Create a base workbook with IronCalc, then inject a fake chart part to simulate Excel extras.
    let mut sheet = make_sheet(
        "Loadsheet",
        &[
            ("A1", json!("ZFW MAX")),
            ("B1", json!(90000)),
            ("A2", json!("DOUBLE")),
            ("B2", json!("=B1*2")),
        ],
    );
    sheet.fills.insert("A1:B1".into(), "#FFD966".into());
    let book = Book {
        title: "Airfast Test".into(),
        sheets: vec![sheet],
        ..Book::default()
    };
    let raw_xlsx = tmp.path().join("orig_raw.xlsx");
    SheetModel::from_book(&book)
        .unwrap()
        .save_xlsx(&raw_xlsx)
        .unwrap();

    // Inject a dummy chart XML entry into the original zip.
    let orig_bytes = std::fs::read(&raw_xlsx).unwrap();
    let with_extra = tmp.path().join("airfast_with_chart.xlsx");
    {
        let mut reader = ZipArchive::new(Cursor::new(&orig_bytes)).unwrap();
        let mut writer = ZipWriter::new(std::fs::File::create(&with_extra).unwrap());
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for i in 0..reader.len() {
            let mut f = reader.by_index(i).unwrap();
            let name = f.name().unwrap().into_owned();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).unwrap();
            writer.start_file(&name, opts).unwrap();
            writer.write_all(&buf).unwrap();
        }
        writer.start_file("xl/charts/chart1.xml", opts).unwrap();
        writer.write_all(b"<chartSpace/>").unwrap();
        writer.finish().unwrap();
    }

    // 2. Import into project — creates lean book.json + style.json + import-report.md.
    let rep = import(tmp.path(), &with_extra).unwrap();
    assert!(rep.dir.join("style.json").exists());
    assert!(rep.dir.join("import-report.md").exists());

    // 3. Edit B1 from 90000 → 30000 and save.
    let item = WorkItem::open(&rep.dir).unwrap();
    let Source::Book(mut loaded) = item.load().unwrap() else {
        panic!("expected book");
    };
    loaded.sheets[0].cells.insert("B1".into(), json!(30000));
    loaded.sheets[0].bold.push("A2".into());
    loaded.sheets[0].fills.insert("B2".into(), "#FF0000".into());
    item.save(&Source::Book(loaded)).unwrap();

    // 4. Write-back export preserves `xl/charts/chart1.xml` AND writes updated B1=30000.
    let exported = export(&item, ExportFormat::Xlsx).unwrap();
    let out_bytes = std::fs::read(&exported[0]).unwrap();
    let mut out_zip = ZipArchive::new(Cursor::new(&out_bytes)).unwrap();
    assert!(
        out_zip.by_name("xl/charts/chart1.xml").is_ok(),
        "write-back must keep original chart parts"
    );

    // Re-importing the write-back xlsx yields B1=30000 and B2=60000.
    let re = import(tmp.path(), &exported[0]).unwrap();
    let re_item = WorkItem::open(&re.dir).unwrap();
    let Source::Book(re_book) = re_item.load().unwrap() else {
        panic!("expected book");
    };
    let re_model = SheetModel::from_book(&re_book).unwrap();
    assert_eq!(re_model.value(0, 1, 2), CellValue::Number(30_000.0));
    assert_eq!(re_model.value(0, 2, 2), CellValue::Number(60_000.0));
    // Look edits are written back too; untouched formats stay.
    let style = |r, c| re_model.model().get_style_for_cell(0, r, c).unwrap();
    let fill = |r, c| format!("{:?}", style(r, c).fill.color).to_uppercase();
    assert!(style(2, 1).font.b, "A2 bold must be written back");
    assert!(fill(2, 2).contains("FF0000"), "B2 fill: {}", fill(2, 2));
    assert!(fill(1, 1).contains("FFD966"), "A1 fill: {}", fill(1, 1));
    assert!(!style(1, 1).font.b);

    // 5. Clean export works too and rebuilds via IronCalc (no chart1.xml).
    let clean_out = export_clean(&item, ExportFormat::Xlsx).unwrap();
    let clean_bytes = std::fs::read(&clean_out[0]).unwrap();
    let mut clean_zip = ZipArchive::new(Cursor::new(&clean_bytes)).unwrap();
    assert!(clean_zip.by_name("xl/charts/chart1.xml").is_err());
}
