//! `/api/v1/rpc`: the allowlisted projection of [`HarnessCore`] the phone may call.
//!
//! `params` is the serde JSON of the matching `harness_core_api` request type (camelCase, as every
//! API type already is). Methods taking plain arguments instead use a small object with the same
//! camelCase names (`sessionId`, `turnId`, …). `shutdown`, `import_*`, `archive_*` and anything
//! else outside [`ALLOWED`] are rejected here.

use harness_core_api::{
    AgentId, ApiResult, HarnessCore, ListProjects, ListSessions, PermissionRequestId, SendMessage,
    SessionId, TurnId,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

/// The only methods a remote client may call (contract §"RPC allowlist").
pub(crate) const ALLOWED: &[&str] = &[
    "list_agents",
    "get_fallback_order",
    "list_sessions",
    "open_session",
    "send_message",
    "stop",
    "cancel_queued",
    "respond_permission",
    "set_active_agent",
    "set_mode",
    "set_model",
    "rename_session",
    "set_pinned",
    "list_projects",
    "turn_diff",
    "list_modes",
    "list_models",
    "list_config_options",
    "set_config_option",
    "settings_overview",
    "settings_page",
];

pub(crate) fn settings_call(
    view: Option<&dyn crate::SettingsView>,
    method: &str,
    params: &Value,
) -> Option<Value> {
    match method {
        "settings_overview" => Some(view.map_or_else(
            || json!({ "host": "", "items": {} }),
            crate::SettingsView::overview,
        )),
        "settings_page" => {
            let page = params
                .get("page")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(view.map_or_else(
                || json!({ "title": page, "subtitle": "", "sections": [] }),
                |view| view.page(page),
            ))
        }
        _ => None,
    }
}

/// True when `method` is allowlisted.
pub(crate) fn is_allowed(method: &str) -> bool {
    ALLOWED.contains(&method)
}

/// Calls one allowlisted method. `Err` carries the message sent to the client.
#[allow(clippy::too_many_lines)]
pub(crate) async fn call(
    core: &dyn HarnessCore,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    match method {
        "list_agents" => out(core.list_agents().await),
        "get_fallback_order" => out(core.get_fallback_order().await),
        "list_sessions" => {
            let request: ListSessions = parse(params)?;
            out(core.list_sessions(request).await)
        }
        "open_session" => open_session(core, params).await,
        "list_modes" => out(core.list_modes(session(params)?).await),
        "list_models" => out(core.list_models(session(params)?).await),
        "list_config_options" => out(core.list_config_options(session(params)?).await),
        "set_config_option" => {
            let request: ConfigChange = strict(params)?;
            out(core
                .set_config_option(
                    SessionId::from(request.session_id.as_str()),
                    request.config_id,
                    request.value,
                )
                .await)
        }
        "send_message" => {
            // `agent`, `clientMessageId` and `context` may be omitted by a small client.
            let filled = fill(
                params,
                json!({ "agent": Value::Null, "clientMessageId": Value::Null, "context": [] }),
            );
            let request: SendMessage = strict(filled)?;
            out(core.send_message(request).await)
        }
        "stop" => out(core.stop(session(params)?).await),
        "cancel_queued" => {
            let (session, turn) = turn(params)?;
            out(core.cancel_queued(session, turn).await)
        }
        "respond_permission" => {
            let request: Permission = strict(params)?;
            out(core
                .respond_permission(
                    SessionId::from(request.session_id.as_str()),
                    PermissionRequestId::from(request.request.as_str()),
                    request.option,
                )
                .await)
        }
        "set_active_agent" => {
            let request: AgentChange = strict(params)?;
            out(core
                .set_active_agent(
                    SessionId::from(request.session_id.as_str()),
                    AgentId::from(request.agent.as_str()),
                )
                .await)
        }
        "set_mode" => {
            let request: ModeChange = strict(params)?;
            out(core
                .set_mode(
                    SessionId::from(request.session_id.as_str()),
                    request.mode_id,
                )
                .await)
        }
        "set_model" => {
            let request: ModelChange = strict(params)?;
            out(core
                .set_model(
                    SessionId::from(request.session_id.as_str()),
                    request.model_id,
                )
                .await)
        }
        "rename_session" => {
            let request: Rename = strict(params)?;
            out(core
                .rename_session(SessionId::from(request.session_id.as_str()), request.title)
                .await)
        }
        "set_pinned" => {
            let request: Pinned = strict(params)?;
            out(core
                .set_pinned(
                    request
                        .session_ids
                        .iter()
                        .map(|id| SessionId::from(id.as_str()))
                        .collect(),
                    request.pinned,
                )
                .await)
        }
        "list_projects" => {
            let request: ListProjects = parse(params)?;
            out(core.list_projects(request).await)
        }
        "turn_diff" => {
            let (session, turn) = turn(params)?;
            out(core.turn_diff(session, turn).await)
        }
        other => Err(format!("method not allowed: {other}")),
    }
}

async fn open_session(core: &dyn HarnessCore, mut params: Value) -> Result<Value, String> {
    let tail = params
        .as_object_mut()
        .and_then(|object| object.remove("tail"))
        .and_then(|value| value.as_u64());
    let detail = out(core.open_session(session(params)?).await)?;
    Ok(tail_turns(detail, tail))
}

fn tail_turns(mut detail: Value, tail: Option<u64>) -> Value {
    let Some(keep) = tail.map(|n| usize::try_from(n).unwrap_or(usize::MAX)) else {
        return detail;
    };
    let mut omitted = 0;
    if let Some(turns) = detail.get_mut("turns").and_then(Value::as_array_mut) {
        turns.sort_by_key(|turn| turn.get("seq").and_then(Value::as_u64).unwrap_or(0));
        if turns.len() > keep {
            omitted = turns.len() - keep;
            turns.drain(..omitted);
        }
    }
    if let Some(object) = detail.as_object_mut() {
        object.insert("omittedTurns".to_owned(), Value::from(omitted));
    }
    detail
}

fn out<T: serde::Serialize>(result: ApiResult<T>) -> Result<Value, String> {
    let value = result.map_err(|error| error.to_string())?;
    serde_json::to_value(value).map_err(|error| format!("encode error: {error}"))
}

/// Deserializes `params`, filling missing keys from the type defaults (`{}` is a valid request).
fn parse<T>(params: Value) -> Result<T, String>
where
    T: DeserializeOwned + Default + serde::Serialize,
{
    let defaults =
        serde_json::to_value(T::default()).map_err(|error| format!("invalid params: {error}"))?;
    strict(fill(params, defaults))
}

/// Overlays the keys of `given` on top of `defaults` (both must be objects to merge).
fn fill(given: Value, defaults: Value) -> Value {
    match (defaults, given) {
        (defaults, Value::Null) => defaults,
        (Value::Object(mut base), Value::Object(given)) => {
            for (key, value) in given {
                base.insert(key, value);
            }
            Value::Object(base)
        }
        (_, given) => given,
    }
}

fn strict<T: DeserializeOwned>(params: Value) -> Result<T, String> {
    if params.is_null() {
        return Err("invalid params: expected an object".to_owned());
    }
    serde_json::from_value(params).map_err(|error| format!("invalid params: {error}"))
}

/// Methods taking one `SessionId` accept `{"sessionId":"…"}` and a bare `"…"` string.
fn session(params: Value) -> Result<SessionId, String> {
    if let Value::String(id) = &params {
        if id.is_empty() {
            return Err("invalid params: empty session id".to_owned());
        }
        return Ok(SessionId::from(id.as_str()));
    }
    let request: SessionOnly = strict(params)?;
    Ok(SessionId::from(request.session_id.as_str()))
}

fn turn(params: Value) -> Result<(SessionId, TurnId), String> {
    let request: TurnRef = strict(params)?;
    Ok((
        SessionId::from(request.session_id.as_str()),
        TurnId::from(request.turn_id.as_str()),
    ))
}

/// Error payload shape used by `/api/v1/rpc` when the body itself is unusable.
pub(crate) fn bad_request(message: &str) -> Value {
    json!({ "ok": false, "error": message })
}

/// A missing or unknown method, as the client sees it.
pub(crate) fn not_allowed(method: &str) -> Value {
    json!({ "ok": false, "error": format!("method not allowed: {method}") })
}

/// Parameters of the scalar-signature methods. Every field also accepts its `snake_case` spelling, so
/// a client may send either `sessionId` (matching the serde convention of the API types) or
/// `session_id`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionOnly {
    #[serde(alias = "session_id")]
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnRef {
    #[serde(alias = "session_id")]
    session_id: String,
    #[serde(alias = "turn_id")]
    turn_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Permission {
    #[serde(alias = "session_id")]
    session_id: String,
    #[serde(rename = "requestId", alias = "request_id")]
    request: String,
    #[serde(rename = "optionId", alias = "option_id")]
    option: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentChange {
    #[serde(alias = "session_id")]
    session_id: String,
    agent: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModeChange {
    #[serde(alias = "session_id")]
    session_id: String,
    #[serde(alias = "mode_id")]
    mode_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigChange {
    #[serde(alias = "session_id")]
    session_id: String,
    #[serde(alias = "config_id")]
    config_id: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelChange {
    #[serde(alias = "session_id")]
    session_id: String,
    #[serde(alias = "model_id")]
    model_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rename {
    #[serde(alias = "session_id")]
    session_id: String,
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pinned {
    #[serde(alias = "session_ids")]
    session_ids: Vec<String>,
    pinned: bool,
}

#[cfg(test)]
mod tests;
