//! Editable rows of the phone's Settings pages and the allowlist `set_setting` checks against, plus the
//! plan-limit mirror the phone's Usage tab reads. A row carries `setting` (the dotted path) and either
//! `on` (toggle), `options` + `current` (choice) or `input: "number"` + range (stepper).

use std::sync::RwLock;

use harness_settings::Settings;
use serde_json::{json, Value};

/// Paths a phone may write. Nothing here holds a credential, a program path or a network address.
const EDITABLE: &[&str] = &[
    "general.language",
    "general.composerSend",
    "general.followUp",
    "general.launchAtStartup",
    "general.showReasoning",
    "general.showTodos",
    "general.groupExploration",
    "general.groupTerminal",
    "general.groupFileChanges",
    "general.autoCompact",
    "general.autoArchive",
    "appearance.theme",
    "appearance.density",
    "appearance.uiFontSize",
    "appearance.zoomPercent",
    "appearance.codeFontSize",
    "appearance.lineNumbers",
    "appearance.wrapLines",
    "appearance.reduceMotion",
    "appearance.showTokenUsage",
    "notifications.taskDone",
    "notifications.buildDone",
    "notifications.prReview",
    "notifications.sound",
    "notifications.keepAwake",
    "notifications.closeToTray",
    "orchestra.defaultMode",
    "orchestra.maxConcurrent",
    "orchestra.autoClaim",
    "orchestra.snapToGrid",
    "memory.workspaceMemoryEnabled",
];

/// Numeric settings: (path, min, max, step).
const RANGES: &[(&str, i64, i64, i64)] = &[
    ("appearance.uiFontSize", 12, 15, 1),
    ("appearance.zoomPercent", 80, 150, 10),
    ("appearance.codeFontSize", 10, 20, 1),
    ("orchestra.maxConcurrent", 1, 32, 1),
];

static LIMITS: RwLock<Value> = RwLock::new(Value::Null);

/// True when a phone may write `path` (`agents.<known id>.enabled` included).
pub(super) fn editable(settings: &Settings, path: &str) -> bool {
    if EDITABLE.contains(&path) {
        return true;
    }
    path.strip_prefix("agents.")
        .and_then(|rest| rest.strip_suffix(".enabled"))
        .is_some_and(|id| settings.agents.contains_key(id))
}

/// Rejects numbers outside the row's range (the stepper enforces it too, but the phone is untrusted).
pub(super) fn check_range(path: &str, value: &Value) -> Result<(), String> {
    let Some(&(_, min, max, step)) = RANGES.iter().find(|(p, ..)| *p == path) else {
        return Ok(());
    };
    let n = value
        .as_i64()
        .ok_or_else(|| format!("{path} needs a whole number"))?;
    if n < min || n > max || (n - min) % step != 0 {
        return Err(format!("{path} must be between {min} and {max}"));
    }
    Ok(())
}

pub(super) fn toggle(title: &str, path: &str, on: bool) -> Value {
    json!({ "title": title, "kind": "toggle", "on": on, "setting": path })
}

pub(super) fn choice(title: &str, path: &str, current: &Value, options: &[(Value, &str)]) -> Value {
    let label = options
        .iter()
        .find(|(value, _)| value == current)
        .map_or_else(|| current.to_string(), |(_, label)| (*label).to_owned());
    let options: Vec<Value> = options
        .iter()
        .map(|(value, label)| json!({ "value": value, "label": label }))
        .collect();
    json!({ "title": title, "value": label, "current": current, "setting": path, "options": options })
}

pub(super) fn number(title: &str, path: &str, current: i64, unit: &str) -> Value {
    let (min, max, step) = RANGES
        .iter()
        .find(|(p, ..)| *p == path)
        .map_or((0, i64::MAX, 1), |&(_, min, max, step)| (min, max, step));
    let shown = if unit.is_empty() {
        current.to_string()
    } else {
        format!("{current} {unit}")
    };
    json!({
        "title": title, "value": shown, "current": current, "setting": path, "input": "number",
        "min": min, "max": max, "step": step, "unit": unit,
    })
}

/// Called by `plan_usage` after each refresh: `[{id, label, plan, error, sessionUsed, …}]`.
pub fn publish_limits(limits: Value) {
    if let Ok(mut slot) = LIMITS.write() {
        *slot = limits;
    }
}

pub(super) fn limits() -> Value {
    LIMITS
        .read()
        .ok()
        .map(|slot| slot.clone())
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_editable_path_exists() {
        let settings = Settings::default();
        for path in EDITABLE {
            assert!(
                crate::settings::value_at(&settings, path).is_some(),
                "{path}"
            );
        }
        assert!(!editable(&settings, "remote.relayUrl"));
        assert!(!editable(&settings, "agents.nope.program"));
    }

    #[test]
    fn ranges_are_enforced() {
        assert!(check_range("appearance.zoomPercent", &json!(110)).is_ok());
        assert!(check_range("appearance.zoomPercent", &json!(115)).is_err());
        assert!(check_range("appearance.zoomPercent", &json!("110")).is_err());
        assert!(check_range("orchestra.maxConcurrent", &json!(0)).is_err());
        assert!(check_range("general.showTodos", &json!(true)).is_ok());
    }
}
