use super::{
    AgentsUi, AppWindow, ComponentHandle, Duration, ModelRc, SharedString, VecModel, EPOCH, MODELS,
};

pub(super) fn clear_models(ui: &AgentsUi<'_>) {
    EPOCH.with(|e| e.set(e.get() + 1));
    MODELS.with(|m| m.borrow_mut().clear());
    ui.set_provider_models(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));
    ui.set_provider_models_note(SharedString::new());
    ui.set_provider_models_loading(false);
}

/// Lists the endpoint's models on a worker thread (`GET <url>/models` with the first key).
pub(super) fn fetch(window: &AppWindow) {
    let ui = window.global::<AgentsUi>();
    let url = ui
        .get_provider_url()
        .trim()
        .trim_end_matches('/')
        .to_owned();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        ui.set_provider_models_note("Enter the endpoint first.".into());
        return;
    }
    let key = first_key(&ui.get_provider_key());
    let epoch = EPOCH.with(|e| {
        e.set(e.get() + 1);
        e.get()
    });
    ui.set_provider_models_loading(true);
    ui.set_provider_models_note("Listing models…".into());
    let weak = window.as_weak();
    let spawned = std::thread::Builder::new()
        .name("harness-list-models".into())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| rt.block_on(list_models(&url, key.as_deref())));
            let _ = slint::invoke_from_event_loop(move || {
                let Some(window) = weak.upgrade() else {
                    return;
                };
                if EPOCH.with(std::cell::Cell::get) != epoch {
                    return;
                }
                let ui = window.global::<AgentsUi>();
                ui.set_provider_models_loading(false);
                match result {
                    Ok(models) => {
                        ui.set_provider_models_note(
                            if models.is_empty() {
                                "The endpoint lists no models; type the model id.".to_owned()
                            } else {
                                format!("{} models: pick one, or type to filter", models.len())
                            }
                            .into(),
                        );
                        let fill = ui.get_provider_model().is_empty() && models.len() == 1;
                        if fill {
                            ui.set_provider_model(models[0].as_str().into());
                        }
                        MODELS.with(|m| *m.borrow_mut() = models);
                        // Show the whole list first; the model field filters it as the user types.
                        show_models(&ui, "");
                    }
                    Err(error) => ui.set_provider_models_note(error.into()),
                }
            });
        });
    if spawned.is_err() {
        ui.set_provider_models_loading(false);
        ui.set_provider_models_note("Could not start the request.".into());
    }
}

fn first_key(keys: &str) -> Option<String> {
    keys.lines()
        .map(str::trim)
        .find(|k| !k.is_empty())
        .map(str::to_owned)
}

const SHOWN_MAX: usize = 200;

pub(super) fn show_models(ui: &AgentsUi<'_>, filter: &str) {
    let shown: Vec<SharedString> = MODELS.with(|m| {
        filtered(&m.borrow(), filter)
            .into_iter()
            .take(SHOWN_MAX)
            .map(SharedString::from)
            .collect()
    });
    ui.set_provider_models(ModelRc::new(VecModel::from(shown)));
}

/// Models whose id contains every word of `filter` (case-insensitive); all of them when `filter` is empty or
/// is exactly one of the ids (a model was just picked).
pub(crate) fn filtered<'a>(models: &'a [String], filter: &str) -> Vec<&'a str> {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() || models.iter().any(|m| m.to_lowercase() == filter) {
        return models.iter().map(String::as_str).collect();
    }
    let words: Vec<&str> = filter.split_whitespace().collect();
    models
        .iter()
        .filter(|m| {
            let m = m.to_lowercase();
            words.iter().all(|w| m.contains(w))
        })
        .map(String::as_str)
        .collect()
}

async fn list_models(url: &str, key: Option<&str>) -> Result<Vec<String>, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut request = client.get(format!("{url}/models"));
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.map_err(|e| {
        if e.is_connect() {
            format!("Cannot reach {url}. Is the server running?")
        } else if e.is_timeout() {
            "The endpoint did not answer in time.".to_owned()
        } else {
            format!("Request failed: {e}")
        }
    })?;
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(if key.is_some() {
            "The endpoint refused this key.".to_owned()
        } else {
            "The endpoint needs a key: paste it above, then list the models.".to_owned()
        });
    }
    if !status.is_success() {
        return Err(format!(
            "The endpoint answered {status} to GET /models; type the model id instead."
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "The model list is not JSON; type the model id instead.".to_owned())?;
    Ok(model_ids(&body))
}

/// Model ids of a `/models` answer: `OpenAI`'s `{"data": [{"id"}]}`, or a bare list, or Ollama's
/// `{"models": [{"name"}]}`. Sorted, duplicates dropped.
pub(crate) fn model_ids(body: &serde_json::Value) -> Vec<String> {
    let list = body
        .get("data")
        .or_else(|| body.get("models"))
        .unwrap_or(body)
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut ids: Vec<String> = list
        .iter()
        .filter_map(|m| {
            m.as_str()
                .or_else(|| m.get("id").and_then(serde_json::Value::as_str))
                .or_else(|| m.get("name").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
        })
        .filter(|id| !id.trim().is_empty())
        .collect();
    ids.sort_by_key(|id| id.to_lowercase());
    ids.dedup();
    ids
}
