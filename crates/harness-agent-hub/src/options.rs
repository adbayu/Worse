//! Agent modes / models / config options (spec phase-9-parity.md §9.1.5): what agents advertise (cached per
//! agent), what a session chose (`session_prefs`), and how both combine into the API views.

use std::collections::BTreeMap;

use harness_core_api::{
    AgentDefaults, AgentId, ApiError, ApiResult, ConfigOption, ConfigValue, ModeInfo, ModelInfo,
    SessionConfigOptions, SessionModels, SessionModes,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Store `settings` key prefix of the advertisement cache.
pub(crate) const CACHE_PREFIX: &str = "agent_options:";

/// What one agent advertised last (`None` = never advertised that part).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Advertised {
    pub modes: Option<Vec<ModeInfo>>,
    pub current_mode: Option<String>,
    pub models: Option<Vec<ModelInfo>>,
    pub current_model: Option<String>,
    pub config: Option<Vec<ConfigOption>>,
}

impl Advertised {
    /// Merges the parts present in an ACP `session/new` / `session/load` response. Returns true when anything
    /// changed.
    pub(crate) fn merge_response(&mut self, result: &Value) -> bool {
        let before = self.clone();
        if let Some(modes) = result.get("modes").filter(|v| v.is_object()) {
            self.modes = Some(
                list(modes, "availableModes")
                    .filter_map(|m| {
                        Some(ModeInfo {
                            id: str_of(m, "id")?,
                            name: str_of(m, "name").unwrap_or_default(),
                            description: str_of(m, "description"),
                        })
                    })
                    .collect(),
            );
            self.current_mode = str_of(modes, "currentModeId");
        }
        if let Some(models) = result.get("models").filter(|v| v.is_object()) {
            self.models = Some(
                list(models, "availableModels")
                    .filter_map(|m| {
                        Some(ModelInfo {
                            id: str_of(m, "modelId").or_else(|| str_of(m, "id"))?,
                            name: str_of(m, "name").unwrap_or_default(),
                            description: str_of(m, "description"),
                        })
                    })
                    .collect(),
            );
            self.current_model = str_of(models, "currentModelId");
        }
        if let Some(options) = result.get("configOptions").filter(|v| v.is_array()) {
            self.config = Some(parse_config(options));
        }
        *self != before
    }

    /// Applies a `session/update` (`current_mode_update`, `config_option_update`). Returns true when changed.
    pub(crate) fn merge_update(&mut self, update: &Value) -> bool {
        let before = self.clone();
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("current_mode_update") => {
                self.current_mode = str_of(update, "currentModeId");
            }
            Some("config_option_update") => {
                if let Some(options) = update.get("configOptions").filter(|v| v.is_array()) {
                    self.config = Some(parse_config(options));
                }
            }
            _ => {}
        }
        *self != before
    }
}

fn str_of(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn list<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn parse_config(options: &Value) -> Vec<ConfigOption> {
    options
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            Some(ConfigOption {
                id: str_of(o, "id")?,
                name: str_of(o, "name").unwrap_or_default(),
                description: str_of(o, "description"),
                category: str_of(o, "category"),
                current: str_of(o, "currentValue"),
                values: list(o, "options")
                    .filter_map(|v| {
                        Some(ConfigValue {
                            value: str_of(v, "value")?,
                            name: str_of(v, "name").unwrap_or_default(),
                            description: str_of(v, "description"),
                        })
                    })
                    .collect(),
            })
        })
        .collect()
}

/// A session's own choices for one agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Prefs {
    pub mode: Option<String>,
    pub model: Option<String>,
    pub config: BTreeMap<String, String>,
}

impl Prefs {
    /// `self` over `defaults` (for a fresh native session).
    pub(crate) fn over_defaults(&self, defaults: Option<&AgentDefaults>) -> Prefs {
        let Some(defaults) = defaults else {
            return self.clone();
        };
        let mut config = defaults.config.clone();
        config.extend(self.config.clone());
        Prefs {
            mode: self.mode.clone().or_else(|| defaults.mode.clone()),
            model: self.model.clone().or_else(|| defaults.model.clone()),
            config,
        }
    }

    /// ACP calls that bring a native session from `applied` to `self` (only the differing values).
    pub(crate) fn calls_from(&self, applied: &Prefs, native: &str) -> Vec<(&'static str, Value)> {
        let mut calls = Vec::new();
        if let Some(mode) = self
            .mode
            .as_ref()
            .filter(|m| applied.mode.as_ref() != Some(*m))
        {
            calls.push((
                "session/set_mode",
                json!({"sessionId": native, "modeId": mode}),
            ));
        }
        if let Some(model) = self
            .model
            .as_ref()
            .filter(|m| applied.model.as_ref() != Some(*m))
        {
            calls.push((
                "session/set_model",
                json!({"sessionId": native, "modelId": model}),
            ));
        }
        for (id, value) in &self.config {
            if applied.config.get(id) != Some(value) {
                calls.push((
                    "session/set_config_option",
                    json!({"sessionId": native, "configId": id, "value": value}),
                ));
            }
        }
        calls
    }
}

/// One `set_*` request.
#[derive(Debug, Clone)]
pub(crate) enum PrefChange {
    Mode(String),
    Model(String),
    Config(String, String),
}

impl PrefChange {
    /// Validates against what the agent advertised (unknown lists accept anything).
    pub(crate) fn validate(&self, advertised: &Advertised) -> ApiResult<()> {
        let bad = |what: String| Err(ApiError::InvalidArgument(what));
        match self {
            Self::Mode(id) => match &advertised.modes {
                Some(modes) if !modes.iter().any(|m| &m.id == id) => {
                    bad(format!("unknown mode {id}"))
                }
                _ if id.trim().is_empty() => bad("mode id is empty".into()),
                _ => Ok(()),
            },
            Self::Model(id) => match &advertised.models {
                Some(models) if !models.iter().any(|m| &m.id == id) => {
                    bad(format!("unknown model {id}"))
                }
                _ if id.trim().is_empty() => bad("model id is empty".into()),
                _ => Ok(()),
            },
            Self::Config(id, value) => {
                let Some(options) = &advertised.config else {
                    return if id.trim().is_empty() {
                        bad("config option id is empty".into())
                    } else {
                        Ok(())
                    };
                };
                let Some(option) = options.iter().find(|o| &o.id == id) else {
                    return bad(format!("unknown config option {id}"));
                };
                if !option.values.is_empty() && !option.values.iter().any(|v| &v.value == value) {
                    return bad(format!("invalid value {value} for {id}"));
                }
                Ok(())
            }
        }
    }

    pub(crate) fn apply(&self, prefs: &mut Prefs) {
        match self {
            Self::Mode(id) => prefs.mode = Some(id.clone()),
            Self::Model(id) => prefs.model = Some(id.clone()),
            Self::Config(id, value) => {
                prefs.config.insert(id.clone(), value.clone());
            }
        }
    }

    /// `(kind, key, value)` row of `session_prefs`.
    pub(crate) fn row(&self) -> (&'static str, &str, &str) {
        match self {
            Self::Mode(id) => ("mode", "", id),
            Self::Model(id) => ("model", "", id),
            Self::Config(id, value) => ("config", id, value),
        }
    }
}

/// API views of one (agent, session prefs, defaults).
pub(crate) fn modes_view(
    agent: AgentId,
    advertised: &Advertised,
    prefs: &Prefs,
    defaults: Option<&AgentDefaults>,
) -> SessionModes {
    SessionModes {
        current: prefs
            .mode
            .clone()
            .or_else(|| defaults.and_then(|d| d.mode.clone()))
            .or_else(|| advertised.current_mode.clone()),
        available: advertised.modes.clone().unwrap_or_default(),
        known: advertised.modes.is_some(),
        agent,
    }
}

pub(crate) fn models_view(
    agent: AgentId,
    advertised: &Advertised,
    prefs: &Prefs,
    defaults: Option<&AgentDefaults>,
) -> SessionModels {
    SessionModels {
        current: prefs
            .model
            .clone()
            .or_else(|| defaults.and_then(|d| d.model.clone()))
            .or_else(|| advertised.current_model.clone()),
        available: advertised.models.clone().unwrap_or_default(),
        known: advertised.models.is_some(),
        agent,
    }
}

pub(crate) fn config_view(
    agent: AgentId,
    advertised: &Advertised,
    prefs: &Prefs,
    defaults: Option<&AgentDefaults>,
) -> SessionConfigOptions {
    let options = advertised
        .config
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|mut o| {
            if let Some(v) = prefs
                .config
                .get(&o.id)
                .or_else(|| defaults.and_then(|d| d.config.get(&o.id)))
            {
                o.current = Some(v.clone());
            }
            o
        })
        .collect();
    SessionConfigOptions {
        agent,
        options,
        known: advertised.config.is_some(),
    }
}

#[cfg(test)]
mod tests;
