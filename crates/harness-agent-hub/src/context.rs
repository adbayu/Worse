//! Context items of a user turn: attachments and mentions (phase-9-parity.md §9.2.1 "Attachments and
//! mentions"). Validation at send, image data to files, and ACP prompt blocks per agent capabilities.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use harness_core_api::{
    chat::limits, ApiError, ApiResult, BlockKind, ContextItem, ResolvedItem, SessionId, TurnId,
};
use serde_json::{json, Value};

use crate::hub::HubCtx;
use crate::store;

/// ACP `promptCapabilities` of an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caps {
    pub image: bool,
    pub embedded: bool,
}

impl Caps {
    /// Used while an agent has not run yet: the real decision is taken when the prompt is built.
    pub(crate) const UNKNOWN: Self = Self {
        image: true,
        embedded: true,
    };
}

pub(crate) fn absolute(workspace: &str, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        Path::new(workspace).join(candidate)
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    format!("file:///{}", text.trim_start_matches('/'))
}

fn decoded_len(data: &str) -> u64 {
    let len = u64::try_from(data.trim().len()).unwrap_or(u64::MAX);
    len / 4 * 3
}

async fn is_small_text(path: &Path, len: u64) -> bool {
    if len > limits::MAX_EMBED_BYTES {
        return false;
    }
    match tokio::fs::read(path).await {
        Ok(bytes) => !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok(),
        Err(_) => false,
    }
}

/// Checks every item without sending (`resolve_context`, and the send-time validation).
pub(crate) async fn check(
    ctx: &HubCtx,
    workspace: &str,
    session: &SessionId,
    items: &[ContextItem],
    caps: Caps,
) -> Vec<ResolvedItem> {
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let mut row = ResolvedItem {
            index: u32::try_from(index).unwrap_or(u32::MAX),
            ok: true,
            error: None,
            block_kind: BlockKind::Text,
            bytes: 0,
            label: String::new(),
        };
        let error = check_one(ctx, workspace, session, item, caps, &mut row).await;
        if let Some(error) = error {
            row.ok = false;
            row.error = Some(error);
        }
        out.push(row);
    }
    out
}

async fn check_one(
    ctx: &HubCtx,
    workspace: &str,
    session: &SessionId,
    item: &ContextItem,
    caps: Caps,
    row: &mut ResolvedItem,
) -> Option<String> {
    match item {
        ContextItem::File { path } => {
            let full = absolute(workspace, path);
            row.label = file_name(&full);
            row.block_kind = BlockKind::ResourceLink;
            match tokio::fs::metadata(&full).await {
                Ok(meta) if meta.is_file() => {
                    row.bytes = meta.len();
                    if caps.embedded && is_small_text(&full, meta.len()).await {
                        row.block_kind = BlockKind::Resource;
                    }
                    None
                }
                Ok(_) => Some(format!("{path} is not a file")),
                Err(_) => Some(format!("{path} does not exist")),
            }
        }
        ContextItem::Folder { path } => {
            let full = absolute(workspace, path);
            row.label = file_name(&full);
            row.block_kind = BlockKind::ResourceLink;
            match tokio::fs::metadata(&full).await {
                Ok(meta) if meta.is_dir() => None,
                Ok(_) => Some(format!("{path} is not a folder")),
                Err(_) => Some(format!("{path} does not exist")),
            }
        }
        ContextItem::Image {
            path,
            data_base64,
            mime,
        } => {
            row.block_kind = if caps.image {
                BlockKind::Image
            } else {
                BlockKind::Unsupported
            };
            if !limits::IMAGE_MIMES.contains(&mime.as_str()) {
                return Some(format!("image type {mime} is not supported"));
            }
            if let Some(data) = data_base64 {
                row.label = "pasted image".into();
                row.bytes = decoded_len(data);
            } else if let Some(path) = path {
                let full = absolute(workspace, path);
                row.label = file_name(&full);
                match tokio::fs::metadata(&full).await {
                    Ok(meta) if meta.is_file() => row.bytes = meta.len(),
                    _ => return Some(format!("{path} does not exist")),
                }
            } else {
                return Some("image has neither a path nor data".into());
            }
            (row.bytes > limits::MAX_IMAGE_BYTES).then(|| {
                format!(
                    "image is larger than {} MB",
                    limits::MAX_IMAGE_BYTES / 1024 / 1024
                )
            })
        }
        ContextItem::Text { title, text } => {
            row.label = title.clone().unwrap_or_else(|| "pasted text".into());
            row.bytes = u64::try_from(text.len()).unwrap_or(u64::MAX);
            (row.bytes > limits::MAX_PASTED_BYTES).then(|| {
                format!(
                    "pasted text is larger than {} MB",
                    limits::MAX_PASTED_BYTES / 1024 / 1024
                )
            })
        }
        ContextItem::Session { session_id } => {
            if session_id == session {
                return Some("a session cannot mention itself".into());
            }
            let lookup = session_id.clone();
            match ctx
                .store
                .run(move |c| store::load_session(c, &lookup))
                .await
            {
                Ok(Some(row_found)) => {
                    row.label = row_found.title;
                    None
                }
                _ => Some(format!("session {session_id} does not exist")),
            }
        }
        ContextItem::Research => {
            row.label = crate::research::LABEL.into();
            None
        }
        ContextItem::CodeMode => {
            row.label = "Code mode".into();
            None
        }
    }
}

/// Send-time validation (`InvalidArgument` naming the item index).
pub(crate) async fn validate(
    ctx: &HubCtx,
    workspace: &str,
    session: &SessionId,
    items: &[ContextItem],
) -> ApiResult<()> {
    if items.len() > limits::MAX_ITEMS {
        return Err(ApiError::InvalidArgument(format!(
            "at most {} context items per message",
            limits::MAX_ITEMS
        )));
    }
    let rows = check(ctx, workspace, session, items, Caps::UNKNOWN).await;
    if let Some(bad) = rows.iter().find(|r| !r.ok) {
        return Err(ApiError::InvalidArgument(format!(
            "context item {}: {}",
            bad.index,
            bad.error.clone().unwrap_or_default()
        )));
    }
    let total: u64 = rows.iter().map(|r| r.bytes).sum();
    if total > limits::MAX_TOTAL_BYTES {
        return Err(ApiError::InvalidArgument(format!(
            "context items are larger than {} MB in total",
            limits::MAX_TOTAL_BYTES / 1024 / 1024
        )));
    }
    Ok(())
}

/// Writes pasted image data to `<home>/attachments/<turn>/<n>.<ext>`; the stored items keep only the path.
pub(crate) async fn store_images(
    home: &Path,
    turn: &TurnId,
    items: Vec<ContextItem>,
) -> ApiResult<Vec<ContextItem>> {
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        match item {
            ContextItem::Image {
                data_base64: Some(data),
                mime,
                ..
            } => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data.trim())
                    .map_err(|e| ApiError::InvalidArgument(format!("context item {index}: {e}")))?;
                let ext = mime.rsplit('/').next().unwrap_or("bin");
                let dir = home.join("attachments").join(turn.as_str());
                tokio::fs::create_dir_all(&dir)
                    .await
                    .map_err(|e| ApiError::Storage(e.to_string()))?;
                let path = dir.join(format!("{index}.{ext}"));
                tokio::fs::write(&path, &bytes)
                    .await
                    .map_err(|e| ApiError::Storage(e.to_string()))?;
                out.push(ContextItem::Image {
                    path: Some(path.to_string_lossy().into_owned()),
                    data_base64: None,
                    mime,
                });
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// ACP content blocks of the items for an agent with `caps` (built per attempt, so failover adapts).
pub(crate) async fn blocks(
    ctx: &HubCtx,
    workspace: &str,
    items: &[ContextItem],
    caps: Caps,
    agent_name: &str,
) -> Vec<Value> {
    let mut out = Vec::new();
    for item in items {
        match item {
            ContextItem::File { path } => {
                let full = absolute(workspace, path);
                let len = tokio::fs::metadata(&full).await.map_or(0, |m| m.len());
                if caps.embedded && is_small_text(&full, len).await {
                    let text = tokio::fs::read_to_string(&full).await.unwrap_or_default();
                    out.push(json!({"type": "resource", "resource": {
                        "uri": file_uri(&full), "mimeType": "text/plain", "text": text}}));
                } else {
                    out.push(json!({"type": "resource_link", "uri": file_uri(&full),
                        "name": file_name(&full), "size": len}));
                }
            }
            ContextItem::Folder { path } => {
                let full = absolute(workspace, path);
                out.push(json!({"type": "resource_link", "uri": file_uri(&full),
                    "name": file_name(&full), "mimeType": "inode/directory"}));
            }
            ContextItem::Image {
                path,
                data_base64,
                mime,
            } => {
                let name = path
                    .as_deref()
                    .map_or_else(|| "pasted image".to_owned(), |p| file_name(Path::new(p)));
                if !caps.image {
                    out.push(json!({"type": "text",
                        "text": format!("[image {name} omitted: {agent_name} does not accept images]")}));
                    continue;
                }
                let data = match (data_base64, path) {
                    (Some(data), _) => Some(data.clone()),
                    (None, Some(path)) => tokio::fs::read(absolute(workspace, path))
                        .await
                        .ok()
                        .map(|b| base64::engine::general_purpose::STANDARD.encode(b)),
                    (None, None) => None,
                };
                if let Some(data) = data {
                    out.push(json!({"type": "image", "mimeType": mime, "data": data}));
                } else {
                    let text = format!("[image {name} omitted: the file is no longer readable]");
                    out.push(json!({"type": "text", "text": text}));
                }
            }
            ContextItem::Text { title, text } => {
                let title = title.clone().unwrap_or_default().replace('"', "'");
                out.push(json!({"type": "text",
                    "text": format!("<pasted title=\"{title}\">\n{text}\n</pasted>")}));
            }
            ContextItem::Session { session_id } => {
                out.push(
                    json!({"type": "text", "text": session_transcript(ctx, session_id).await}),
                );
            }
            ContextItem::Research => {
                out.push(json!({"type": "text", "text": crate::research::INSTRUCTIONS}));
                out.push(json!({"type": "text", "text": crate::research_figures::INSTRUCTIONS}));
                if ctx.config.fact_check.on() {
                    out.push(json!({"type": "text", "text": crate::factcheck::INSTRUCTIONS}));
                }
            }
            ContextItem::CodeMode => {}
        }
    }
    out
}

/// User/assistant text of another session, newest turns kept, at most 32 KB.
async fn session_transcript(ctx: &HubCtx, session: &SessionId) -> String {
    let lookup = session.clone();
    let loaded = ctx
        .store
        .run(move |c| {
            Ok((
                store::load_session(c, &lookup)?,
                store::load_turns(c, &lookup)?,
            ))
        })
        .await;
    let Ok((Some(row), turns)) = loaded else {
        return format!("[session {session} is no longer available]");
    };
    let mut parts: Vec<String> = Vec::new();
    let mut used = 0usize;
    for (turn, _) in turns.iter().rev() {
        let part = format!(
            "User: {}\nAssistant: {}\n",
            turn.user_text, turn.assistant_text
        );
        if used + part.len() > limits::MAX_SESSION_BYTES {
            break;
        }
        used += part.len();
        parts.push(part);
    }
    parts.reverse();
    let title = row.title.replace('"', "'");
    format!(
        "<session id=\"{session}\" title=\"{title}\">\n{}</session>",
        parts.concat()
    )
}
