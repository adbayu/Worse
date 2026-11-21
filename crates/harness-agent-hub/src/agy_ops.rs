//! Antigravity account operations on the hub: one at a time, agy processes stopped before a login swap,
//! credential store calls off the async runtime.

use std::collections::HashSet;
use std::sync::Arc;

use harness_agent_config::AgentKind;
use harness_core_api::{AgentId, AgyAccountOp, AgyAccounts, ApiError, ApiResult};

use crate::agy_accounts::{self as accounts, AgyFile};
use crate::agy_cred::{slot_target, AgyCredStore};
use crate::hub::HubCtx;
use crate::util::now_ms;

#[derive(Default)]
pub(crate) struct AgyState {
    pub(crate) lock: tokio::sync::Mutex<()>,
    exhausted: std::sync::Mutex<HashSet<String>>,
}

impl AgyState {
    fn exhausted(&self) -> HashSet<String> {
        self.exhausted.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn mark(&self, id: &str, on: bool) {
        if let Ok(mut set) = self.exhausted.lock() {
            if on {
                set.insert(id.to_owned());
            } else {
                set.remove(id);
            }
        }
    }
}

type Op<T> = Box<dyn FnOnce(&dyn AgyCredStore, &mut AgyFile) -> Result<T, String> + Send>;

async fn blocking<T: Send + 'static>(
    store: Arc<dyn AgyCredStore>,
    mut file: AgyFile,
    op: Op<T>,
) -> ApiResult<(AgyFile, T)> {
    tokio::task::spawn_blocking(move || {
        let out = op(store.as_ref(), &mut file);
        out.map(|value| (file, value))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .map_err(ApiError::InvalidArgument)
}

pub(crate) fn is_agy(ctx: &HubCtx, agent: &AgentId) -> bool {
    agent.as_str() == "agy" || ctx.config.agent_kinds.get(agent) == Some(&AgentKind::Agy)
}

pub(crate) async fn stop_agy(ctx: &HubCtx) {
    let ids: Vec<AgentId> = ctx
        .specs
        .keys()
        .filter(|id| is_agy(ctx, id))
        .cloned()
        .collect();
    for id in ids {
        ctx.pool.stop_agent(&id).await;
    }
}

fn store_of(ctx: &HubCtx) -> Arc<dyn AgyCredStore> {
    Arc::clone(&ctx.config.agy_creds.0)
}

async fn render(ctx: &HubCtx, file: AgyFile) -> ApiResult<AgyAccounts> {
    let exhausted = ctx.agy.exhausted();
    let store = store_of(ctx);
    tokio::task::spawn_blocking(move || accounts::view(store.as_ref(), &file, &exhausted))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Runs one account operation and answers with the fresh list.
pub(crate) async fn run(ctx: &HubCtx, op: AgyAccountOp) -> ApiResult<AgyAccounts> {
    let store = store_of(ctx);
    if !store.available() {
        return match op {
            AgyAccountOp::List => Ok(AgyAccounts::default()),
            _ => Err(ApiError::AgentUnavailable(
                "Antigravity accounts need Windows Credential Manager (Windows only)".into(),
            )),
        };
    }
    let _guard = ctx.agy.lock.lock().await;
    let home = ctx.config.home.clone();
    let mut file = accounts::load(&home).await?;
    file.normalize();
    let before = file.clone();
    let mut same_as = None;
    let (file, ()) = match op {
        AgyAccountOp::List => (file, ()),
        // 修复：之前每次 List（包括额度轮询）都会自动完成等待中的登录，旧登录被悄悄存进新账号；现在只在点 Done 时完成。
        AgyAccountOp::FinishSignIn => {
            let (file, finish) = blocking(store, file, Box::new(accounts::finish_sign_in)).await?;
            if let accounts::Finish::SameAs(name) = finish {
                same_as = Some(name);
            }
            (file, ())
        }
        AgyAccountOp::Add { name } => {
            let now = now_ms();
            file.add(&name, now).map_err(ApiError::InvalidArgument)?;
            (file, ())
        }
        AgyAccountOp::Remove { id } => {
            ctx.agy.mark(&id, false);
            blocking(
                store,
                file,
                Box::new(move |s, f| accounts::remove(s, f, &id)),
            )
            .await?
        }
        AgyAccountOp::SetOrder { order } => {
            file.set_order(order);
            (file, ())
        }
        AgyAccountOp::Import { name } => {
            let now = now_ms();
            let (file, id) = blocking(
                store,
                file,
                Box::new(move |s, f| accounts::import_live(s, f, &name, now)),
            )
            .await?;
            ctx.agy.mark(&id, false);
            (file, ())
        }
        AgyAccountOp::SignIn { id } => {
            stop_agy(ctx).await;
            blocking(
                store,
                file,
                Box::new(move |s, f| accounts::begin_sign_in(s, f, &id)),
            )
            .await?
        }
        AgyAccountOp::Use { id } => {
            let probe = Arc::clone(&store);
            let target = slot_target(&id);
            let has = tokio::task::spawn_blocking(move || probe.exists(&target))
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?
                .map_err(ApiError::Storage)?;
            if !has {
                return Err(ApiError::InvalidArgument(
                    "this account has no saved login; click Sign in first".into(),
                ));
            }
            stop_agy(ctx).await;
            ctx.agy.mark(&id, false);
            blocking(
                store,
                file,
                Box::new(move |s, f| accounts::use_account(s, f, &id)),
            )
            .await?
        }
    };
    if file != before {
        accounts::save(&home, &file).await?;
    }
    if let Some(name) = same_as {
        return Err(ApiError::InvalidArgument(format!(
            "agy is signed in to the same Google account as {name}. Sign out of Google in the browser (or use \
             a private window), then press Sign in again and pick the other account."
        )));
    }
    render(ctx, file).await
}

/// On an agy usage limit: marks the active account exhausted and swaps in the next account of the fallback
/// order that has a login. True when agy now runs on another account (the caller retries the turn).
pub(crate) async fn rotate_on_quota(ctx: &HubCtx) -> bool {
    let store = store_of(ctx);
    if !store.available() {
        return false;
    }
    let _guard = ctx.agy.lock.lock().await;
    let home = ctx.config.home.clone();
    let Ok(mut file) = accounts::load(&home).await else {
        return false;
    };
    file.normalize();
    let Some(active) = file.active.clone() else {
        return false;
    };
    ctx.agy.mark(&active, true);
    let exhausted = ctx.agy.exhausted();
    let probe = Arc::clone(&store);
    let pick = file.clone();
    let next = tokio::task::spawn_blocking(move || {
        pick.next_fallback(
            &|id| probe.exists(&slot_target(id)).unwrap_or(false),
            &exhausted,
        )
    })
    .await
    .ok()
    .flatten();
    let Some(next) = next else {
        return false;
    };
    stop_agy(ctx).await;
    let swapped = blocking(
        store,
        file,
        Box::new(move |s, f| accounts::use_account(s, f, &next)),
    )
    .await;
    match swapped {
        Ok((file, ())) => {
            if let Err(error) = accounts::save(&home, &file).await {
                tracing::warn!(%error, "agy_accounts.json not saved after failover");
            }
            tracing::info!("agy usage limit: switched to the next Antigravity account");
            true
        }
        Err(error) => {
            tracing::warn!(%error, "agy account failover failed");
            false
        }
    }
}
