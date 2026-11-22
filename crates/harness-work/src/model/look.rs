//! `style.json` next to `book.json`: the presentation of imported workbooks (fills, borders,
//! fonts, sizes, merges, row heights, …) lives there so `book.json` stays small for agents.
//! Loading merges it back per sheet; books without `style.json` keep everything in `book.json`.

use serde_json::{Map, Value};

/// Sheet keys that move to `style.json`.
const KEYS: [&str; 17] = [
    "widths",
    "merges",
    "fills",
    "bold",
    "colors",
    "align",
    "borders",
    "sizes",
    "italic",
    "underline",
    "wrap",
    "borderColors",
    "heights",
    "hiddenRows",
    "hiddenCols",
    "notes",
    "links",
];

pub(crate) const FILE: &str = "style.json";
const FORMAT: &str = "harness-style/1";

/// Moves presentation keys out of a serialized book; returns `style.json` text.
pub(crate) fn split(book: &mut Value) -> String {
    let mut sheets = Map::new();
    if let Some(list) = book.get_mut("sheets").and_then(Value::as_array_mut) {
        for sheet in list {
            let Some(obj) = sheet.as_object_mut() else {
                continue;
            };
            let name = obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut look = Map::new();
            for key in KEYS {
                if let Some(v) = obj.remove(key) {
                    look.insert(key.to_string(), v);
                }
            }
            sheets.insert(name, Value::Object(look));
        }
    }
    let mut out = Map::new();
    out.insert("format".into(), Value::String(FORMAT.into()));
    out.insert("sheets".into(), Value::Object(sheets));
    serde_json::to_string(&Value::Object(out)).unwrap_or_default()
}

/// Puts `style.json` keys back into the sheets of a parsed book (book values win).
pub(crate) fn merge(book: &mut Value, style: &str) {
    let Ok(style) = serde_json::from_str::<Value>(style) else {
        return;
    };
    let Some(looks) = style.get("sheets").and_then(Value::as_object) else {
        return;
    };
    let Some(list) = book.get_mut("sheets").and_then(Value::as_array_mut) else {
        return;
    };
    for sheet in list {
        let Some(obj) = sheet.as_object_mut() else {
            continue;
        };
        let name = obj.get("name").and_then(Value::as_str).unwrap_or_default();
        let Some(look) = looks.get(name).and_then(Value::as_object) else {
            continue;
        };
        for (k, v) in look {
            if !obj.contains_key(k) {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_and_merge_round_trip() {
        let original = json!({"format": "harness-book/1", "sheets": [
            {"name": "S", "cells": {"A1": 1}, "fills": {"A1": "#FF0000"}, "formats": {"A1": "0"}}
        ]});
        let mut book = original.clone();
        let style = split(&mut book);
        assert!(book["sheets"][0].get("fills").is_none());
        assert!(book["sheets"][0].get("formats").is_some());
        merge(&mut book, &style);
        assert_eq!(book, original);
    }
}
