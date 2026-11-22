use super::*;

fn val(v: Value) -> Edit {
    Edit {
        value: Some(v),
        style: None,
    }
}

fn look(xf: usize) -> Edit {
    Edit {
        value: None,
        style: Some(xf),
    }
}

fn sheet_book(cells: &[(&str, Value)]) -> Book {
    let mut sheet = crate::model::book::Sheet {
        name: "S".into(),
        ..Default::default()
    };
    for (k, v) in cells {
        sheet.cells.insert((*k).into(), v.clone());
    }
    Book {
        sheets: vec![sheet],
        ..Book::default()
    }
}

#[test]
fn keeps_cells_and_rows_in_order() {
    let cells = [
        ("A2", 1.into()),
        ("B2", 5.into()),
        ("D2", 2.into()),
        ("A4", 7.into()),
        ("B9", "x".into()),
    ];
    let book = sheet_book(&cells);
    let model = SheetModel::from_book(&book).unwrap();
    let ctx = Ctx {
        model: &model,
        idx: 0,
        cells: Some(&book.sheets[0].cells),
    };
    let xml = r#"<worksheet><sheetData><row r="2"><c r="A2"><v>1</v></c><c r="D2"><v>2</v></c></row><row r="3"/><row r="5"><c r="A5"><v>3</v></c></row></sheetData></worksheet>"#;
    let mut rows = Changes::new();
    rows.entry(2).or_default().insert(2, val(5.into()));
    rows.entry(3).or_default().insert(1, val("y".into()));
    rows.entry(4).or_default().insert(1, val(7.into()));
    rows.entry(9).or_default().insert(2, val("x".into()));
    rows.entry(5).or_default().insert(1, val(Value::Null));
    let out = patch_sheet(xml, &rows, &ctx).unwrap();
    let expect = r#"<row r="2"><c r="A2"><v>1</v></c><c r="B2"><v>5</v></c><c r="D2"><v>2</v></c></row><row r="3"><c r="A3" t="inlineStr"><is><t xml:space="preserve">y</t></is></c></row><row r="4"><c r="A4"><v>7</v></c></row><row r="5"></row><row r="9"><c r="B9" t="inlineStr"><is><t xml:space="preserve">x</t></is></c></row></sheetData>"#;
    assert!(out.contains(expect), "{out}");
}

#[test]
fn restyles_cells_in_place() {
    let book = sheet_book(&[("A1", 1.into()), ("B1", 9.into())]);
    let model = SheetModel::from_book(&book).unwrap();
    let ctx = Ctx {
        model: &model,
        idx: 0,
        cells: Some(&book.sheets[0].cells),
    };
    let xml = r#"<worksheet><sheetData><row r="1"><c r="A1" s="2"><v>1</v></c><c r="B1"><v>3</v></c></row></sheetData></worksheet>"#;
    let mut rows = Changes::new();
    rows.entry(1).or_default().insert(1, look(7));
    let both = Edit {
        value: Some(9.into()),
        style: Some(8),
    };
    rows.entry(1).or_default().insert(2, both);
    rows.entry(1).or_default().insert(3, look(7));
    let out = patch_sheet(xml, &rows, &ctx).unwrap();
    let expect = r#"<row r="1"><c r="A1" s="7"><v>1</v></c><c r="B1" s="8"><v>9</v></c><c r="C1" s="7"/></row>"#;
    assert!(out.contains(expect), "{out}");
    assert_eq!(set_style(r#"<c r="A1"/>"#, "4"), r#"<c s="4" r="A1"/>"#);
}
