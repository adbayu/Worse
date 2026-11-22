use super::*;
use crate::model::book::Sheet;
use serde_json::json;

fn budget() -> Book {
    let mut s = Sheet {
        name: "Budget".into(),
        ..Sheet::default()
    };
    for (k, v) in [
        ("A1", json!("Item")),
        ("B1", json!("Qty")),
        ("C1", json!("Price")),
        ("D1", json!("Total")),
        ("A2", json!("Licences")),
        ("B2", json!(40)),
        ("C2", json!(30)),
        ("D2", json!("=B2*C2")),
        ("A3", json!("Support")),
        ("B3", json!(1)),
        ("C3", json!(900)),
        ("D3", json!("=B3*C3")),
        ("D4", json!("=SUM(D2:D3)")),
        ("E1", json!("=1/0")),
    ] {
        s.cells.insert(k.into(), v);
    }
    s.bold.push("A1:D1".into());
    s.fills.insert("A1:D1".into(), "#E5E7EB".into());
    s.conditional.push(Conditional {
        range: "D2:D3".into(),
        when: ">1000".into(),
        fill: "#FEE2E2".into(),
    });
    s.freeze = Some("A2".into());
    Book {
        sheets: vec![s],
        ..Book::default()
    }
}

#[test]
fn evaluates_and_formats() {
    let m = SheetModel::from_book(&budget()).unwrap();
    assert_eq!(m.formatted(0, 4, 4), "2100");
    let g = m.grid(0, 1, 1, 4, 5);
    assert!(g.rows[0][0].bold);
    assert_eq!(g.rows[0][0].fill.as_deref(), Some("#E5E7EB"));
    assert_eq!(g.rows[1][3].fill.as_deref(), Some("#FEE2E2"));
    assert!(g.rows[0][4].error);
    assert_eq!(g.frozen, (1, 0));
    assert_eq!(g.rows[3][3].formula, "=SUM(D2:D3)");
}

#[test]
fn edit_recalculates_and_exports() {
    let mut m = SheetModel::from_book(&budget()).unwrap();
    m.set(0, 3, 2, "2").unwrap();
    assert_eq!(m.formatted(0, 4, 4), "3000");
    assert!(m.csv(0).starts_with("Item,Qty,Price,Total"));
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("b.xlsx");
    m.save_xlsx(&p).unwrap();
    assert!(p.metadata().unwrap().len() > 1000);
    let r = m.range("Budget!A1:B2").unwrap();
    assert_eq!(r.len(), 2);
}
