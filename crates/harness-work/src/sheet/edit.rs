//! Structural edits on an evaluated sheet model: batch cell sets without rebuilding, formula
//! fill with relative references, and row/column insert/delete with reference shifting.

use std::collections::BTreeMap;

use ironcalc::base::cell::CellValue;
use ironcalc::base::Model;
use serde_json::Value;

use super::{cell_name, sheet_err, SheetModel};
use crate::error::{WorkError, WorkResult};
use crate::model::book::{cell_input, parse_cell, Book};

/// The `book.json` value of a cell: its formula (`=…`) or its typed value.
pub(crate) fn source_value(model: &Model, sheet: u32, row: i32, col: i32) -> Option<Value> {
    if let Ok(Some(f)) = model.get_cell_formula(sheet, row, col) {
        return Some(Value::String(if f.starts_with('=') {
            f
        } else {
            format!("={f}")
        }));
    }
    match model.get_cell_value_by_index(sheet, row, col).ok()? {
        CellValue::None => None,
        CellValue::String(s) if s.is_empty() => None,
        // 以 = 开头的普通文本前加 ' ，避免被当作公式。
        CellValue::String(s) if s.starts_with('=') => Some(Value::String(format!("'{s}"))),
        CellValue::String(s) => Some(Value::String(s)),
        CellValue::Number(n) => serde_json::Number::from_f64(n).map(Value::Number),
        CellValue::Boolean(b) => Some(Value::Bool(b)),
    }
}

impl SheetModel {
    /// Sets several cells (`Value::Null` or `""` clears), then recalculates once.
    pub fn set_cells(&mut self, edits: &[(u32, i32, i32, Value)]) -> WorkResult<()> {
        for (sheet, r, c, v) in edits {
            let s = usize::try_from(*sheet)
                .ok()
                .and_then(|i| self.book.sheets.get_mut(i))
                .ok_or_else(|| WorkError::Invalid(format!("no sheet {sheet}")))?;
            let key = cell_name(*r, *c);
            if v.is_null() || v.as_str() == Some("") {
                s.cells.remove(&key);
                self.model
                    .set_user_input(*sheet, *r, *c, String::new())
                    .map_err(sheet_err)?;
            } else {
                s.cells.insert(key, v.clone());
                self.model
                    .set_user_input(*sheet, *r, *c, cell_input(v))
                    .map_err(sheet_err)?;
            }
        }
        self.model.evaluate();
        Ok(())
    }

    /// The formula of (`r`, `c`) as it reads when copied to (`tr`, `tc`).
    pub fn extend_to(&self, sheet: u32, r: i32, c: i32, tr: i32, tc: i32) -> WorkResult<String> {
        self.model.extend_to(sheet, r, c, tr, tc).map_err(sheet_err)
    }

    /// Inserts (`n > 0`) or deletes (`n < 0`) rows (`rows = true`) or columns at `at`. References
    /// in every sheet follow; the model is rebuilt from the resulting book.
    pub fn restructure(&mut self, sheet: u32, rows: bool, at: i32, n: i32) -> WorkResult<()> {
        if n == 0 || at < 1 {
            return Ok(());
        }
        let m = &mut self.model;
        match (rows, n > 0) {
            (true, true) => m.insert_rows(sheet, at, n),
            (true, false) => m.delete_rows(sheet, at, -n),
            (false, true) => m.insert_columns(sheet, at, n),
            (false, false) => m.delete_columns(sheet, at, -n),
        }
        .map_err(sheet_err)?;
        let mut book: Book = self.book.clone();
        for s in &mut book.sheets {
            s.cells.clear();
        }
        for cell in self.model.get_all_cells() {
            let Some(s) = book.sheets.get_mut(cell.index as usize) else {
                continue;
            };
            if let Some(v) = source_value(&self.model, cell.index, cell.row, cell.column) {
                s.cells.insert(cell_name(cell.row, cell.column), v);
            }
        }
        if let Some(s) = usize::try_from(sheet)
            .ok()
            .and_then(|i| book.sheets.get_mut(i))
        {
            crate::model::shift::sheet(s, rows, at, n);
        }
        let mut names = BTreeMap::new();
        for (name, scope, formula) in self.model.get_defined_name_list() {
            let key = match scope.and_then(|i| book.sheets.get(i as usize)) {
                Some(s) => format!("{}!{name}", s.name),
                None => name,
            };
            names.insert(key, formula);
        }
        book.names = names;
        *self = SheetModel::from_book(&book)?;
        Ok(())
    }

    /// Formula cells whose value is an error: (sheet, row, col, shown, formula).
    #[must_use]
    pub fn errors(&self) -> Vec<(u32, i32, i32, String, String)> {
        let mut out = Vec::new();
        for (i, s) in self.book.sheets.iter().enumerate() {
            let Ok(idx) = u32::try_from(i) else { continue };
            for key in s.cells.keys() {
                let Some((r, c)) = parse_cell(key) else {
                    continue;
                };
                let Some(f) = self.formula(idx, r, c) else {
                    continue;
                };
                let shown = self.formatted(idx, r, c);
                if shown.starts_with('#') && shown.ends_with(['!', '?', 'A']) {
                    out.push((idx, r, c, shown, f));
                }
            }
        }
        out
    }
}
