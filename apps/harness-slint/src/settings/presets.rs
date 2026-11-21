//! Ready-made API providers for "Add provider": picking one fills the endpoint and a default model, so the
//! user only pastes a key. Every preset is an OpenAI-compatible Chat Completions endpoint (Harness's own API
//! agent speaks that); providers that serve Claude or their best models only on another API are noted.
//!
//! Endpoints follow each provider's documentation and the provider tables of hermes-agent
//! (`hermes_cli/auth.py`) and oh-my-pi (`packages/catalog/src/provider-models/openai-compat.ts`). Default
//! models are a starting point only: model ids change often, so the dialog lists the endpoint's own models
//! (`GET <endpoint>/models`) once a key is in.

use std::time::Duration;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::ui::{AgentsUi, AppWindow, ProviderPreset};

mod catalog;
mod models;

pub(super) use catalog::{find, Preset, PRESETS};
use models::{clear_models, fetch, show_models};

/// The dialog's provider kind for a preset: the Command Code CLI preset makes CLI-key agents.
pub(super) fn kind_of(id: &str) -> &'static str {
    if id == "command-code-cli" {
        "command-code"
    } else {
        ""
    }
}

thread_local! {
    /// The full model list of the last fetch; the dialog shows it filtered by the model field.
    static MODELS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Bumped on every fetch and preset change, so a late answer for an old endpoint is dropped.
    static EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn row(p: &Preset) -> ProviderPreset {
    ProviderPreset {
        id: p.id.into(),
        name: p.name.into(),
        tag: p.tag.into(),
        url: p.url.into(),
        model: p.model.into(),
        note: p.note.into(),
        key_url: p.key_url.into(),
        keyless: p.keyless,
        letter: letter(p.name).into(),
    }
}

/// First letter or digit of a name, upper-cased (`OpenRouter` → `O`).
pub(super) fn letter(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

pub(super) fn install(window: &AppWindow) {
    let ui = window.global::<AgentsUi>();
    let rows: Vec<ProviderPreset> = PRESETS.iter().map(row).collect();
    ui.set_presets(ModelRc::new(VecModel::from(rows)));

    let weak = window.as_weak();
    ui.on_pick_preset(move |id| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let ui = window.global::<AgentsUi>();
        let preset = find(&id);
        ui.set_provider_preset(id.clone());
        ui.set_provider_current(preset.map(row).unwrap_or_default());
        ui.set_provider_kind(kind_of(&id).into());
        ui.set_provider_label(
            preset
                .map_or("", |p| if p.id == "custom" { "" } else { p.name })
                .into(),
        );
        ui.set_provider_url(preset.map_or("", |p| p.url).into());
        ui.set_provider_model(preset.map_or("", |p| p.model).into());
        ui.set_provider_edit_url(id == "custom");
        clear_models(&ui);
        // A local server needs no key: list its models straight away.
        if preset.is_some_and(|p| p.keyless) {
            fetch(&window);
        }
    });

    let weak = window.as_weak();
    ui.on_fetch_models(move || {
        if let Some(window) = weak.upgrade() {
            fetch(&window);
        }
    });

    let weak = window.as_weak();
    ui.on_filter_models(move |text| {
        if let Some(window) = weak.upgrade() {
            show_models(&window.global::<AgentsUi>(), &text);
        }
    });

    ui.on_open_key_page(|url| crate::remote::open_url(&url));
}

#[cfg(test)]
#[allow(clippy::assert_is_empty)]
mod tests {
    use super::models::{filtered, model_ids};
    use super::{find, kind_of, PRESETS};

    #[test]
    fn presets_are_well_formed() {
        let mut ids = Vec::new();
        for p in PRESETS {
            assert!(!ids.contains(&p.id), "duplicate preset {}", p.id);
            ids.push(p.id);
            assert!(!p.name.is_empty());
            if !matches!(p.id, "custom" | "command-code-cli") {
                assert!(
                    p.url.starts_with("https://") || p.url.starts_with("http://127.0.0.1"),
                    "{}",
                    p.id
                );
                assert!(!p.url.ends_with('/'), "{}", p.id);
            }
            if !p.key_url.is_empty() {
                assert!(p.key_url.starts_with("https://"), "{}", p.id);
            }
        }
        assert_eq!(super::letter("OpenRouter"), "O");
        assert_eq!(super::letter("  9router"), "9");
        assert_eq!(kind_of("command-code-cli"), "command-code");
        assert_eq!(kind_of("command-code-api"), "");
        assert_eq!(
            find("command-code-api").map(|p| p.url),
            Some("https://api.commandcode.ai/provider/v1")
        );
    }

    #[test]
    fn reads_openai_ollama_and_bare_lists() {
        let openai = serde_json::json!({"object": "list", "data": [{"id": "b-model"}, {"id": "A-model"}, {"id": "b-model"}]});
        assert_eq!(model_ids(&openai), ["A-model", "b-model"]);
        let ollama = serde_json::json!({"models": [{"name": "qwen3:8b"}]});
        assert_eq!(model_ids(&ollama), ["qwen3:8b"]);
        let bare = serde_json::json!(["x", {"id": "y"}]);
        assert_eq!(model_ids(&bare), ["x", "y"]);
        assert!(model_ids(&serde_json::json!({"error": "nope"})).is_empty());
    }

    #[test]
    fn filter_matches_all_words_and_keeps_list_after_a_pick() {
        let models: Vec<String> = [
            "deepseek/deepseek-v4-flash",
            "openai/gpt-5.5",
            "deepseek/deepseek-v4-pro",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(filtered(&models, "seek pro"), ["deepseek/deepseek-v4-pro"]);
        assert_eq!(filtered(&models, "GPT").len(), 1);
        assert_eq!(filtered(&models, "").len(), 3);
        assert_eq!(filtered(&models, "openai/gpt-5.5").len(), 3);
    }
}
