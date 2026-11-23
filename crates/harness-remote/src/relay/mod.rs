//! Remote Control through a relay (docs/plans/remote-relay.md): the desktop keeps one outbound WebSocket to
//! the relay (a Cloudflare Worker), phones join through it, and every phone is end-to-end encrypted and bridged
//! onto this crate's loopback server. Reconnects with backoff; no port is opened and no tunnel is needed.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::Connector;

mod bridge;
pub(crate) mod crypto;
mod identity;

pub use identity::RelayIdentity;

const PING_EVERY: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Frames queued per phone before it is dropped as too slow.
const PHONE_QUEUE: usize = 256;

/// What the relay connection is doing (Settings › Remote status line).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RelayStatus {
    #[default]
    Off,
    Connecting,
    Online {
        phones: usize,
    },
    Error {
        message: String,
    },
}

/// Frames for the relay writer.
pub(crate) enum Out {
    Frame(u32, Vec<u8>),
    Kick(u32),
}

/// A running relay connection; dropping it does not stop it, call [`RelayHandle::stop`].
pub struct RelayHandle {
    status: watch::Receiver<RelayStatus>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl RelayHandle {
    #[must_use]
    pub fn status(&self) -> RelayStatus {
        self.status.borrow().clone()
    }

    /// Live status for a UI that polls it.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<RelayStatus> {
        self.status.clone()
    }

    /// Closes the connection and waits for the task.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

impl crate::RemoteHandle {
    /// Connects this server to the relay at `relay_url` (`https://…`) as the device in `<state_dir>/relay.json`.
    ///
    /// # Errors
    /// No tokio runtime is current, the URL is not `https://` or the identity file cannot be loaded.
    pub fn start_relay(&self, relay_url: &str) -> std::io::Result<RelayHandle> {
        // 修复：没有 tokio 运行时时返回错误，而不是在 tokio::spawn 里 panic。
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|e| std::io::Error::other(format!("no tokio runtime: {e}")))?;
        let identity = RelayIdentity::load_or_create(&self.inner.state_dir)?;
        if identity.host_url(relay_url, 0).is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the relay URL must start with https://",
            ));
        }
        let (status_tx, status) = watch::channel(RelayStatus::Connecting);
        let (stop, stop_rx) = watch::channel(false);
        let ctx = Ctx {
            identity,
            url: relay_url.to_owned(),
            local: self.local_addr(),
            status: status_tx,
        };
        let task = runtime.spawn(run(ctx, stop_rx));
        Ok(RelayHandle { status, stop, task })
    }

    /// `<HARNESS_HOME>` of this server (where `relay.json` lives).
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.inner.state_dir.clone()
    }
}

struct Ctx {
    identity: RelayIdentity,
    url: String,
    local: SocketAddr,
    status: watch::Sender<RelayStatus>,
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn tls() -> Option<Connector> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .ok()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Some(Connector::Rustls(Arc::new(config)))
}

async fn run(ctx: Ctx, mut stop: watch::Receiver<bool>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if *stop.borrow() {
            break;
        }
        let _ = ctx.status.send(RelayStatus::Connecting);
        let url = ctx.identity.host_url(&ctx.url, now_s()).unwrap_or_default();
        let connector = if url.starts_with("wss://") {
            tls()
        } else {
            None
        };
        let message =
            match tokio_tungstenite::connect_async_tls_with_config(url, None, false, connector)
                .await
            {
                Ok((socket, _)) => {
                    backoff = Duration::from_secs(1);
                    match session(&ctx, socket, &mut stop).await {
                        Ended::Stopped => break,
                        Ended::Replaced => {
                            "another Harness with the same identity connected".to_owned()
                        }
                        Ended::Lost => "connection lost".to_owned(),
                    }
                }
                Err(error) => format!("cannot reach the relay: {error}"),
            };
        let _ = ctx.status.send(RelayStatus::Error {
            message: format!("{message}; retrying in {} s", backoff.as_secs()),
        });
        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            _ = stop.changed() => break,
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
    let _ = ctx.status.send(RelayStatus::Off);
}

enum Ended {
    Stopped,
    Replaced,
    Lost,
}

async fn session<S>(
    ctx: &Ctx,
    socket: tokio_tungstenite::WebSocketStream<S>,
    stop: &mut watch::Receiver<bool>,
) -> Ended
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut sink, mut source) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<Out>(PHONE_QUEUE);
    let mut phones: HashMap<u32, mpsc::Sender<Vec<u8>>> = HashMap::new();
    let mut tasks = tokio::task::JoinSet::new();
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let _ = ctx.status.send(RelayStatus::Online { phones: 0 });
    let ended = loop {
        tokio::select! {
            _ = stop.changed() => {
                let _ = sink.send(Message::Close(None)).await;
                break Ended::Stopped;
            }
            _ = ping.tick() => {
                if sink.send(Message::text("ping")).await.is_err() {
                    break Ended::Lost;
                }
            }
            Some(out) = out_rx.recv() => {
                let message = match out {
                    Out::Frame(cid, payload) => Message::binary([cid.to_be_bytes().as_slice(), &payload].concat()),
                    Out::Kick(cid) => {
                        phones.remove(&cid);
                        Message::text(format!("{{\"t\":\"kick\",\"c\":{cid}}}"))
                    }
                };
                if sink.send(message).await.is_err() {
                    break Ended::Lost;
                }
            }
            incoming = source.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let Ok(control) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                    let cid = control.get("c").and_then(Value::as_u64).and_then(|c| u32::try_from(c).ok());
                    match (control.get("t").and_then(Value::as_str), cid) {
                        // 重复的 open 会让两个任务共用一个 cid，直接忽略。
                        (Some("open"), Some(cid)) if !phones.contains_key(&cid) => {
                            let (tx, rx) = mpsc::channel(PHONE_QUEUE);
                            phones.insert(cid, tx);
                            let secret = *ctx.identity.secret();
                            tasks.spawn(bridge::phone(cid, secret, ctx.local, rx, out_tx.clone()));
                        }
                        (Some("close"), Some(cid)) => {
                            phones.remove(&cid);
                        }
                        (Some("replaced"), _) => break Ended::Replaced,
                        _ => {}
                    }
                    let _ = ctx.status.send(RelayStatus::Online { phones: phones.len() });
                }
                Some(Ok(Message::Binary(data))) if data.len() >= 4 => {
                    let cid = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                    let full = phones.get(&cid).is_some_and(|tx| tx.try_send(data[4..].to_vec()).is_err());
                    if full {
                        phones.remove(&cid);
                        let _ = sink.send(Message::text(format!("{{\"t\":\"kick\",\"c\":{cid}}}"))).await;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break Ended::Lost,
                Some(Ok(_)) => {}
            },
        }
    };
    tasks.abort_all();
    ended
}
