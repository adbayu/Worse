//! Order-preserving JSON tree.
//!
//! `serde_json::Value` sorts object keys (the workspace does not enable `preserve_order`, and enabling it would change
//! serialisation for every crate in the build). Config files written by other programs (`~/.claude.json`,
//! `.mcp.json`, `settings.json`) must keep their key order and indentation when Harness edits one entry, so edits go
//! through [`OValue`]: parsed by `serde_json` in document order, printed with the file's own indentation.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::BuildHasher;
use std::sync::OnceLock;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::{Number, Value};

mod serde_impl;

/// JSON value whose objects keep insertion order. Equality of objects ignores key order (semantic equality, used
/// for "same content → no write").
#[derive(Debug, Clone, Default)]
pub enum OValue {
    #[default]
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<OValue>),
    Object(OMap),
}

/// Ordered JSON object.
#[derive(Debug, Clone, Default)]
pub struct OMap(Vec<(String, OValue)>);

impl PartialEq for OValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::Object(a), Self::Object(b)) => a == b,
            _ => false,
        }
    }
}

impl PartialEq for OMap {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .all(|(k, v)| other.get(k).is_some_and(|w| w == v))
    }
}

impl OMap {
    /// Empty object.
    #[must_use]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Value of `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&OValue> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Mutable value of `key`.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut OValue> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Sets `key`: replaced in place when present (position kept), appended otherwise.
    pub fn insert(&mut self, key: impl Into<String>, value: OValue) {
        let key = key.into();
        if let Some(slot) = self.get_mut(&key) {
            *slot = value;
        } else {
            self.0.push((key, value));
        }
    }

    /// Removes `key`, returning its value.
    pub fn remove(&mut self, key: &str) -> Option<OValue> {
        let index = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(index).1)
    }

    /// Renames `from` to `to` in place (keeps position). No-op when `from` is missing.
    pub fn rename(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        self.remove(to);
        if let Some(entry) = self.0.iter_mut().find(|(k, _)| k == from) {
            to.clone_into(&mut entry.0);
        }
    }

    /// The object for `key`, created (appended) when missing or not an object.
    pub fn object_mut(&mut self, key: &str) -> &mut OMap {
        let index = if let Some(i) = self.0.iter().position(|(k, _)| k == key) {
            i
        } else {
            self.0.push((key.to_owned(), OValue::Object(OMap::new())));
            self.0.len() - 1
        };
        let slot = &mut self.0[index].1;
        if !matches!(slot, OValue::Object(_)) {
            *slot = OValue::Object(OMap::new());
        }
        match slot {
            OValue::Object(map) => map,
            _ => unreachable!("slot was just set to an object"),
        }
    }

    /// Keys in order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }

    /// Entries in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &OValue)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Mutable entries in order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut OValue)> {
        self.0.iter_mut().map(|(k, v)| (k.as_str(), v))
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl OValue {
    /// The object, if this is one.
    #[must_use]
    pub fn as_object(&self) -> Option<&OMap> {
        match self {
            Self::Object(map) => Some(map),
            _ => None,
        }
    }

    /// The mutable object, if this is one.
    pub fn as_object_mut(&mut self) -> Option<&mut OMap> {
        match self {
            Self::Object(map) => Some(map),
            _ => None,
        }
    }

    /// The string, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// Converts from a `serde_json::Value` (object keys in the `Value`'s iteration order).
    #[must_use]
    pub fn from_json(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(b),
            Value::Number(n) => Self::Number(n),
            Value::String(s) => Self::String(s),
            Value::Array(items) => Self::Array(items.into_iter().map(Self::from_json).collect()),
            Value::Object(map) => Self::Object(OMap(
                map.into_iter()
                    .map(|(k, v)| (k, Self::from_json(v)))
                    .collect(),
            )),
        }
    }

    /// Converts to a `serde_json::Value` (loses key order; for typed deserialisation).
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Bool(*b),
            Self::Number(n) => Value::Number(n.clone()),
            Self::String(s) => Value::String(s.clone()),
            Self::Array(items) => Value::Array(items.iter().map(Self::to_json).collect()),
            Self::Object(map) => Value::Object(
                map.0
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect(),
            ),
        }
    }

    /// Parses JSON text.
    ///
    /// # Errors
    ///
    /// Invalid JSON.
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// Pretty-prints with `indent` (e.g. two spaces); `trailing_newline` appends `\n`.
    #[must_use]
    pub fn to_pretty(&self, indent: &str, trailing_newline: bool) -> String {
        let mut out = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
        let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
        // Serialising an in-memory tree into a Vec cannot fail (no I/O, string keys only).
        if self.serialize(&mut serializer).is_err() {
            return String::new();
        }
        let mut text = String::from_utf8(out).unwrap_or_default();
        if trailing_newline {
            text.push('\n');
        }
        text
    }

    /// Deep merge: objects merge key by key (keys of `src` win, other keys of `self` stay in place); any other value
    /// of `src` replaces. `replace_paths` lists dotted paths (relative to the root) whose value `src` replaces
    /// wholesale even when both are objects (user-owned maps such as `shortcuts`).
    pub fn merge_from(&mut self, src: &OValue, replace_paths: &[&str]) {
        merge_at(self, src, "", replace_paths);
    }
}

fn merge_at(dst: &mut OValue, src: &OValue, path: &str, replace_paths: &[&str]) {
    match (dst.as_object_mut(), src) {
        (Some(dst_map), OValue::Object(src_map)) if !replace_paths.contains(&path) => {
            for (key, value) in src_map.iter() {
                let child = if path.is_empty() {
                    key.to_owned()
                } else {
                    format!("{path}.{key}")
                };
                match dst_map.get_mut(key) {
                    Some(existing) => merge_at(existing, value, &child, replace_paths),
                    None => dst_map.insert(key, value.clone()),
                }
            }
        }
        _ => *dst = src.clone(),
    }
}

/// Indentation of a JSON text (first indented line after `{`/`[`), default two spaces.
#[must_use]
pub fn detect_indent(text: &str) -> String {
    let mut previous_opens = false;
    for line in text.lines() {
        if previous_opens {
            let indent: String = line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            if !indent.is_empty() {
                return indent;
            }
        }
        let trimmed = line.trim_end();
        previous_opens = trimmed.ends_with('{') || trimmed.ends_with('[');
    }
    "  ".into()
}

/// Masks a secret value for display: `••••` plus a 4-hex fingerprint from a per-process random key, so a preview can
/// show *that* a value changed without revealing it (the key never leaves the process).
#[must_use]
pub fn mask_secret(value: &str) -> String {
    static KEY: OnceLock<RandomState> = OnceLock::new();
    let hash = KEY.get_or_init(RandomState::new).hash_one(value);
    format!("••••{:04x}", hash & 0xffff)
}

/// True for key names that usually hold secrets (`apiKey`, `token`, `password`, `Authorization`, ...).
#[must_use]
pub fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "key",
        "token",
        "secret",
        "password",
        "passwd",
        "credential",
        "auth",
        "cookie",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Copy of `value` with secrets masked: every string under an object key in `map_keys` (e.g. `env`, `headers`) and
/// every string whose own key looks secret ([`is_secret_key`]).
#[must_use]
pub fn masked(value: &OValue, map_keys: &[&str]) -> OValue {
    mask_inner(value, map_keys, false)
}

fn mask_inner(value: &OValue, map_keys: &[&str], inside_secret_map: bool) -> OValue {
    match value {
        OValue::String(s) if inside_secret_map => OValue::String(mask_secret(s)),
        OValue::Object(map) => OValue::Object(OMap(
            map.0
                .iter()
                .map(|(k, v)| {
                    let secret_map = map_keys.contains(&k.as_str());
                    let child = match v {
                        OValue::String(s) if is_secret_key(k) => OValue::String(mask_secret(s)),
                        _ => mask_inner(v, map_keys, secret_map || inside_secret_map),
                    };
                    (k.clone(), child)
                })
                .collect(),
        )),
        OValue::Array(items) => OValue::Array(
            items
                .iter()
                .map(|i| mask_inner(i, map_keys, inside_secret_map))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests;
