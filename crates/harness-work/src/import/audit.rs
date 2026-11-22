//! Import audit: recomputes every formula with our engine and compares it with the value Excel
//! saved in the file, so the user knows exactly which cells differ and why.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use ironcalc::base::cell::CellValue;

use super::xlsx_extra::Flags;
use crate::model::book::Book;
use crate::sheet::{cell_name, SheetModel};

/// A formula cell's value as Excel saved it: (sheet, row, col, value).
pub(super) type Cached = (u32, i32, i32, CellValue);

/// Functions whose value changes on every recalculation; never reported as differences.
const VOLATILE: [&str; 6] = [
    "TODAY(",
    "NOW(",
    "RAND(",
    "RANDBETWEEN(",
    "RANDARRAY(",
    "INFO(",
];

fn same(a: &CellValue, b: &CellValue) -> bool {
    match (a, b) {
        (CellValue::Number(x), CellValue::Number(y)) => {
            (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0)
        }
        (CellValue::String(x), CellValue::String(y)) => x == y,
        (CellValue::Boolean(x), CellValue::Boolean(y)) => x == y,
        (CellValue::None, CellValue::String(s)) | (CellValue::String(s), CellValue::None) => {
            s.is_empty()
        }
        (CellValue::None, CellValue::Number(n)) | (CellValue::Number(n), CellValue::None) => {
            *n == 0.0
        }
        _ => false,
    }
}

fn show(v: &CellValue) -> String {
    match v {
        CellValue::None => "(empty)".into(),
        CellValue::String(s) => s.clone(),
        CellValue::Number(n) => n.to_string(),
        CellValue::Boolean(b) => b.to_string().to_uppercase(),
    }
}

/// Function names used in a formula (`SUM`, `XLOOKUP`, …).
fn functions(formula: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for ch in formula.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            word.push(ch);
        } else {
            if ch == '('
                && word
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            {
                out.push(word.to_ascii_uppercase());
            }
            word.clear();
        }
    }
    out
}

/// Warnings for the import dialog and the full `import-report.md`.
#[allow(clippy::too_many_lines)]
pub(super) fn run(book: &Book, cached: &[Cached], flags: &Flags) -> (Vec<String>, String) {
    let mut warnings = Vec::new();
    let mut report = format!("# Import report: {}\n\n", book.title);
    let Ok(model) = SheetModel::from_book(book) else {
        warnings.push("The workbook could not be recalculated; values are shown as saved.".into());
        return (warnings, report);
    };
    let mut diffs = Vec::new();
    let mut unknown = BTreeSet::new();
    let mut volatile = 0usize;
    for (sheet, r, c, excel) in cached {
        let formula = model.formula(*sheet, *r, *c).unwrap_or_default();
        let upper = formula.to_ascii_uppercase();
        if VOLATILE.iter().any(|v| upper.contains(v)) {
            volatile += 1;
            continue;
        }
        if matches!(excel, CellValue::None) {
            continue;
        }
        let ours = model.value(*sheet, *r, *c);
        if same(excel, &ours) {
            continue;
        }
        if matches!(&ours, CellValue::String(s) if s == "#NAME?") {
            unknown.extend(functions(&formula));
        }
        let name = model
            .book()
            .sheets
            .get(*sheet as usize)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        diffs.push((
            format!("{name}!{}", cell_name(*r, *c)),
            show(excel),
            show(&ours),
            formula,
        ));
    }
    let checked = cached.len();
    let _ = writeln!(
        report,
        "- Formulas checked against Excel's saved values: {checked}\n- Same result: {}\n- Different: {}\n",
        checked - diffs.len() - volatile,
        diffs.len()
    );
    if diffs.is_empty() {
        if checked > 0 {
            warnings.push(format!(
                "All {checked} formulas give the same result as in Excel."
            ));
        }
    } else {
        let first: Vec<String> = diffs
            .iter()
            .take(5)
            .map(|(cell, excel, ours, _)| format!("{cell} Excel {excel} / here {ours}"))
            .collect();
        warnings.push(format!(
            "{} of {checked} formulas give a different result than in Excel ({}{}). See import-report.md.",
            diffs.len(),
            first.join("; "),
            if diffs.len() > 5 { "; …" } else { "" }
        ));
        report.push_str("## Different from Excel\n\n| Cell | Excel | Here | Formula |\n| --- | --- | --- | --- |\n");
        for (cell, excel, ours, formula) in diffs.iter().take(200) {
            let _ = writeln!(
                report,
                "| {cell} | {excel} | {ours} | `{}` |",
                formula.replace('|', "\\|")
            );
        }
        report.push('\n');
    }
    // 只报告引擎确实不认识的函数名（出现在 #NAME? 单元格中）。
    if !unknown.is_empty() {
        let list: Vec<String> = unknown.into_iter().collect();
        let line = format!(
            "Some formulas give #NAME? here; check these functions or names: {}.",
            list.join(", ")
        );
        let _ = writeln!(report, "## Unknown functions or names\n\n{line}\n");
        warnings.push(line);
    }
    let mut notes = Vec::new();
    if volatile > 0 {
        notes.push(format!(
            "{volatile} formulas use TODAY/NOW/RAND and are recalculated on open (not compared)."
        ));
    }
    if flags.external_links > 0 {
        notes.push(format!(
            "The workbook links to {} other file(s); those references keep Excel's last values or show #REF!.",
            flags.external_links
        ));
    }
    if flags.date1904 {
        notes.push("The workbook uses the 1904 date system; dates here count from 1900, so date serials differ by 1462 days.".into());
    }
    if flags.iterate {
        notes.push("Iterative calculation (circular references) is on in Excel; circular formulas give #CIRC! here.".into());
    }
    if !notes.is_empty() {
        report.push_str("## Workbook notes\n\n");
        for n in &notes {
            let _ = writeln!(report, "- {n}");
        }
        warnings.extend(notes);
    }
    (warnings, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_function_names() {
        assert_eq!(
            functions("=IFERROR(XLOOKUP(A1,B:B,C:C),0)+_xlfn.LET(x,1,x)"),
            vec!["IFERROR", "XLOOKUP", "_XLFN.LET"]
        );
        assert!(same(&CellValue::Number(0.1 + 0.2), &CellValue::Number(0.3)));
    }
}
