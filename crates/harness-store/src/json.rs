//! Ordered JSON output and the JavaScript coercions the TS repositories apply to SQLite values.
//!
//! The TS services build plain objects whose key order is the order the RPC codec serialises
//! (`JSON.stringify`). `serde_json` maps are only order-preserving when the `preserve_order`
//! feature happens to be unified into a build, so results are written as text here.

use std::fmt::Write as _;

use rusqlite::types::ValueRef;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::StoreError;

/// Largest integer node:sqlite returns as a JS number (`Number.MAX_SAFE_INTEGER`); larger values
/// make node:sqlite throw `ERR_OUT_OF_RANGE`, so the TS read fails and Rust must fall back.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// An ordered JSON value. `Undef` mirrors a JS `undefined` property: omitted when written.
#[derive(Debug, Clone)]
pub enum J {
    /// Already-serialised JSON text.
    Raw(String),
    /// JS `undefined`.
    Undef,
    /// Object in insertion order.
    Obj(Obj),
    /// Array.
    Arr(Vec<J>),
}

impl J {
    /// JSON string.
    #[must_use]
    pub fn str(value: &str) -> Self {
        let mut out = String::with_capacity(value.len() + 2);
        write_str(&mut out, value);
        Self::Raw(out)
    }

    /// JSON boolean.
    #[must_use]
    pub fn bool(value: bool) -> Self {
        Self::Raw(if value { "true" } else { "false" }.to_owned())
    }

    /// JSON `null`.
    #[must_use]
    pub fn null() -> Self {
        Self::Raw("null".to_owned())
    }

    /// Serialises to compact JSON text.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Raw(text) => out.push_str(text),
            // JSON.stringify 在数组里把 undefined 写成 null；对象属性在 Obj::write 中直接省略。
            Self::Undef => out.push_str("null"),
            Self::Obj(obj) => obj.write(out),
            Self::Arr(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
        }
    }
}

/// Object with JS property semantics: assigning an existing key keeps its position.
#[derive(Debug, Clone, Default)]
pub struct Obj(Vec<(String, J)>);

impl Obj {
    /// Empty object.
    #[must_use]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// `obj[key] = value` (spread / assignment order semantics).
    pub fn set(&mut self, key: &str, value: J) {
        if let Some(entry) = self.0.iter_mut().find(|(name, _)| name == key) {
            entry.1 = value;
        } else {
            self.0.push((key.to_owned(), value));
        }
    }

    /// Builder form of [`Obj::set`].
    #[must_use]
    pub fn with(mut self, key: &str, value: J) -> Self {
        self.set(key, value);
        self
    }

    /// Own property lookup.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&J> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    fn write(&self, out: &mut String) {
        out.push('{');
        let mut first = true;
        for (key, value) in &self.0 {
            if matches!(value, J::Undef) {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            write_str(out, key);
            out.push(':');
            value.write(out);
        }
        out.push('}');
    }
}

impl From<Obj> for J {
    fn from(obj: Obj) -> Self {
        Self::Obj(obj)
    }
}

fn write_str(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// An owned SQLite value as node:sqlite would hand it to JS.
#[derive(Debug, Clone, PartialEq)]
pub enum Sql {
    /// `null`.
    Null,
    /// INTEGER within the JS safe range.
    Int(i64),
    /// REAL.
    Real(f64),
    /// TEXT.
    Text(String),
    /// BLOB (a `Uint8Array` in JS).
    Blob,
}

impl Sql {
    /// Converts a column value; fails where node:sqlite would throw (unsafe integers) or where
    /// rusqlite cannot represent the TEXT (invalid UTF-8).
    pub fn from_ref(value: ValueRef<'_>) -> Result<Self, StoreError> {
        Ok(match value {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(i) => {
                if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&i) {
                    return Err(StoreError::fallback("integer outside the JS safe range"));
                }
                Self::Int(i)
            }
            ValueRef::Real(f) => Self::Real(f),
            ValueRef::Text(bytes) => Self::Text(
                std::str::from_utf8(bytes)
                    .map_err(|_| StoreError::fallback("TEXT is not UTF-8"))?
                    .to_owned(),
            ),
            ValueRef::Blob(_) => Self::Blob,
        })
    }

    /// `value === 1`.
    #[must_use]
    pub fn is_one(&self) -> bool {
        match self {
            Self::Int(i) => *i == 1,
            #[allow(clippy::float_cmp)]
            Self::Real(f) => *f == 1.0,
            _ => false,
        }
    }

    /// `value == null`.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// `typeof value === "string"`.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// JS truthiness.
    #[must_use]
    pub fn truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Int(i) => *i != 0,
            Self::Real(f) => *f != 0.0 && !f.is_nan(),
            Self::Text(text) => !text.is_empty(),
            Self::Blob => true,
        }
    }

    /// `a === b` for values read from SQLite.
    #[must_use]
    pub fn strict_eq(&self, other: &Self) -> bool {
        #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Int(a), Self::Real(b)) | (Self::Real(b), Self::Int(a)) => (*a as f64) == *b,
            (Self::Real(a), Self::Real(b)) => a == b,
            (Self::Text(a), Self::Text(b)) => a == b,
            _ => false,
        }
    }

    /// `String(value)`; REAL and BLOB are not reproduced (fallback).
    pub fn js_string(&self) -> Result<String, StoreError> {
        match self {
            Self::Null => Ok("null".to_owned()),
            Self::Int(i) => Ok(i.to_string()),
            Self::Text(text) => Ok(text.clone()),
            Self::Real(_) => Err(StoreError::fallback("String() of a REAL value")),
            Self::Blob => Err(StoreError::fallback("String() of a BLOB value")),
        }
    }

    /// `Number(value)` as JSON text; TEXT and BLOB are not reproduced (fallback).
    pub fn js_number(&self) -> Result<J, StoreError> {
        match self {
            Self::Null => Ok(J::Raw("0".to_owned())),
            Self::Int(i) => Ok(J::Raw(i.to_string())),
            Self::Real(f) => Ok(real(*f)),
            Self::Text(_) => Err(StoreError::fallback("Number() of a TEXT value")),
            Self::Blob => Err(StoreError::fallback("Number() of a BLOB value")),
        }
    }

    /// The raw value as JSON (rows returned unchanged, e.g. task groups).
    pub fn raw_json(&self) -> Result<J, StoreError> {
        match self {
            Self::Null => Ok(J::null()),
            Self::Int(i) => Ok(J::Raw(i.to_string())),
            Self::Real(f) => Ok(real(*f)),
            Self::Text(text) => Ok(J::str(text)),
            Self::Blob => Err(StoreError::fallback("BLOB column in a returned row")),
        }
    }
}

/// A finite f64 as JSON number text (Rust's `Display` is shortest round-trip without exponent,
/// which `JSON.parse` reads back to the same double); NaN/Infinity become `null` like
/// `JSON.stringify`.
fn real(value: f64) -> J {
    if value.is_finite() {
        J::Raw(format!("{value}"))
    } else {
        J::null()
    }
}

/// JS truthiness of a JSON parameter (`undefined` when absent).
#[must_use]
pub fn param_truthy(value: Option<&serde_json::Value>) -> bool {
    match value {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// Top-level members of an object, in JS property order (`JSON.parse`: a repeated key keeps its
/// first position and its last value).
struct Members(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for Members {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MembersVisitor;
        impl<'de> Visitor<'de> for MembersVisitor {
            type Value = Members;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Members, A::Error> {
                let mut out: Vec<(String, Box<RawValue>)> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    let value: Box<RawValue> = map.next_value()?;
                    if let Some(entry) = out.iter_mut().find(|(name, _)| *name == key) {
                        entry.1 = value;
                    } else {
                        out.push((key, value));
                    }
                }
                Ok(Members(out))
            }
        }
        deserializer.deserialize_map(MembersVisitor)
    }
}

/// `{...JSON.parse(text)}` as an ordered object. Primitives spread to `{}`; strings and arrays
/// (which spread their indices) and invalid JSON (TS throws) fall back.
pub fn spread_json(text: &str) -> Result<Obj, StoreError> {
    let trimmed = text.trim_start_matches([' ', '\t', '\n', '\r']);
    match trimmed.as_bytes().first() {
        Some(b'{') => {
            let members: Members = serde_json::from_str(text)
                .map_err(|e| StoreError::fallback(format!("meta_json: {e}")))?;
            let mut obj = Obj::new();
            for (key, value) in members.0 {
                obj.set(&key, J::Raw(value.get().to_owned()));
            }
            Ok(obj)
        }
        // TS 先展开 meta 再读 meta.traceId：meta 为 null 时抛 TypeError（整个 readTree 失败），
        // Rust 必须回退让 TS 给出同样的错误，而不能当作空对象返回成功结果。
        Some(b'n') => Err(StoreError::fallback("meta_json is null (TS throws)")),
        Some(b't' | b'f' | b'-' | b'0'..=b'9') => {
            serde_json::from_str::<serde::de::IgnoredAny>(text)
                .map_err(|e| StoreError::fallback(format!("meta_json: {e}")))?;
            Ok(Obj::new())
        }
        _ => Err(StoreError::fallback("meta_json is not an object")),
    }
}

/// Whether a raw member value is `null` (for `??`).
#[must_use]
pub fn is_nullish(value: Option<&J>) -> bool {
    match value {
        None | Some(J::Undef) => true,
        Some(J::Raw(text)) => text == "null",
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests;
