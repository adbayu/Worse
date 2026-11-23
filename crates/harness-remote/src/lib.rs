//! Remote phone access to a running Harness desktop.
//!
//! The desktop app owns the hub; this crate is a second *consumer* of [`HarnessCore`]: it serves a
//! loopback-only HTTP/WebSocket bridge so a phone (through a Cloudflare tunnel) can list sessions,
//! drive turns and answer permissions.
//!
//! Security model (spec `phase-12-remote-phone.md` §3.4): every route except the static shell,
//! `/healthz` and `/api/v1/pair` needs a device token; the token travels as the
//! `harness_remote` cookie (or `Authorization: Bearer`); comparison is constant time; failed
//! pair/auth attempts are rate limited per IP; `Host` and `Origin` are checked; JSON request bodies
//! are capped at 1 MB (16 MB for `/api/v1/rpc`, 25 MB for `/api/v1/upload`); the `/api/v1/rpc` method
//! allowlist never exposes `shutdown`, imports, archives or file writes. Tokens, pair codes and pair
//! links are never logged.

#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use harness_core_api::{HarnessCore, API_VERSION};
use tokio::net::TcpListener;
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

mod auth;
mod http;
mod relay;
mod rpc;
mod ws;

#[cfg(test)]
mod tests;

pub use harness_core_api::API_VERSION as REMOTE_API_VERSION;
pub use relay::{RelayHandle, RelayIdentity, RelayStatus};

/// Server configuration, built by the desktop from `RemoteSettings`.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// TCP port on `127.0.0.1` (default 7878). Never bound on another address.
    pub port: u16,
    /// `<HARNESS_HOME>`; paired devices live in `state_dir/remote.json`.
    pub state_dir: PathBuf,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            port: 7878,
            state_dir: PathBuf::new(),
        }
    }
}

pub trait SettingsView: Send + Sync {
    fn overview(&self) -> serde_json::Value;
    fn page(&self, page: &str) -> serde_json::Value;

    /// Writes one allowlisted setting (`general.showReasoning`, …) from a phone row.
    ///
    /// # Errors
    /// The path is not editable from a phone or the value does not fit the setting.
    fn set(&self, path: &str, value: &serde_json::Value) -> Result<(), String> {
        let _ = (path, value);
        Err("settings cannot be changed from a phone here".to_owned())
    }

    /// Plan limits of the signed-in accounts (`[{id, label, plan, sessionUsed, …}]`).
    fn limits(&self) -> serde_json::Value {
        serde_json::Value::Array(Vec::new())
    }
}

/// One paired device, as shown in Settings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub created_ms: u64,
    pub last_seen_ms: u64,
}

/// A single-use pair offer: a long random token (for the QR / link) plus a short typed code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairOffer {
    /// 64 hex chars.
    pub token: String,
    /// 8 chars of `A-Z2-9`.
    pub code: String,
    /// Unix milliseconds at which the offer stops working (10 minutes).
    pub expires_ms: u64,
}

/// Handle of a running remote server. Cheap to clone; all clones share the same state.
#[derive(Clone)]
pub struct RemoteHandle {
    inner: Arc<Inner>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

/// Shared server state.
pub(crate) struct Inner {
    pub(crate) core: Arc<dyn HarnessCore>,
    pub(crate) auth: auth::Auth,
    /// `<HARNESS_HOME>`; phone uploads land in `state_dir/remote-uploads/<yyyymmdd>/`.
    pub(crate) state_dir: PathBuf,
    pub(crate) public_host: RwLock<Option<String>>,
    pub(crate) local_addr: SocketAddr,
    pub(crate) last_seq: AtomicU64,
    /// Flips to `true` on shutdown; every connection selects on it.
    pub(crate) closing: watch::Sender<bool>,
    pub(crate) settings: RwLock<Option<Arc<dyn SettingsView>>>,
}

impl Inner {
    /// Highest `seq` this server has forwarded so far (used by the WS `hello` frame).
    pub(crate) fn seq(&self) -> u64 {
        self.last_seq.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn set_seq(&self, seq: u64) {
        self.last_seq
            .fetch_max(seq, std::sync::atomic::Ordering::Relaxed);
    }

    /// True when `host` is an accepted `Host` header value: the configured public host, or the
    /// loopback forms of this server. Blocks DNS rebinding.
    pub(crate) fn host_allowed(&self, host: &str) -> bool {
        let host = host.trim();
        if host.is_empty() {
            return false;
        }
        if let Ok(public) = self.public_host.read() {
            if let Some(public) = public.as_deref() {
                if host.eq_ignore_ascii_case(public) {
                    return true;
                }
            }
        }
        let port = self.local_addr.port();
        host.eq_ignore_ascii_case(&format!("127.0.0.1:{port}"))
            || host.eq_ignore_ascii_case(&format!("localhost:{port}"))
            || host.eq_ignore_ascii_case(&format!("[::1]:{port}"))
    }

    /// True when a `POST` `Origin` header (if any) belongs to this server.
    pub(crate) fn origin_allowed(&self, origin: &str) -> bool {
        let Some(rest) = origin
            .strip_prefix("https://")
            .or_else(|| origin.strip_prefix("http://"))
        else {
            return false;
        };
        self.host_allowed(rest)
    }

    pub(crate) fn closing_rx(&self) -> watch::Receiver<bool> {
        self.closing.subscribe()
    }
}

impl RemoteHandle {
    /// Binds `127.0.0.1:<port>` and spawns the server. Returns once the socket is bound.
    ///
    /// # Errors
    ///
    /// The bind error of the loopback socket (port taken, permission denied).
    pub async fn start(cfg: RemoteConfig, core: Arc<dyn HarnessCore>) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, cfg.port)).await?;
        let local_addr = listener.local_addr()?;
        let (closing, _) = watch::channel(false);
        let inner = Arc::new(Inner {
            core,
            auth: auth::Auth::load(&cfg.state_dir),
            state_dir: cfg.state_dir.clone(),
            public_host: RwLock::new(None),
            local_addr,
            last_seq: AtomicU64::new(0),
            closing,
            settings: RwLock::new(None),
        });
        let accept = Arc::clone(&inner);
        let task = tokio::spawn(async move { http::accept_loop(accept, listener).await });
        Ok(Self {
            inner,
            task: Arc::new(Mutex::new(Some(task))),
        })
    }

    pub fn set_settings_view(&self, view: Arc<dyn SettingsView>) {
        if let Ok(mut slot) = self.inner.settings.write() {
            *slot = Some(view);
        }
    }

    /// The bound loopback address (port 0 in the config resolves here).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr
    }

    /// Sets the tunnel host name (e.g. `harness.example.com`); `None` allows loopback hosts only.
    pub fn set_public_host(&self, host: Option<String>) {
        let mut slot = self
            .inner
            .public_host
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = host
            .map(|host| host.trim().to_owned())
            .filter(|host| !host.is_empty());
    }

    /// Creates a single-use pair offer (10 minutes) for a device name.
    #[must_use]
    pub fn new_pair_offer(&self, device_name: &str) -> PairOffer {
        self.inner.auth.new_offer(device_name)
    }

    /// Paired devices, oldest first.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceInfo> {
        self.inner.auth.devices()
    }

    /// Removes a device and closes its WebSocket connections.
    pub fn revoke(&self, id: &str) {
        self.inner.auth.revoke(id);
    }

    /// Stops the listener and closes every connection.
    pub async fn shutdown(&self) {
        let _ = self.inner.closing.send(true);
        if let Some(task) = self.task.lock().await.take() {
            task.abort();
        }
    }

    /// `api_version` reported by `/api/v1/status`.
    #[must_use]
    pub fn api_version(&self) -> &'static str {
        API_VERSION
    }
}
