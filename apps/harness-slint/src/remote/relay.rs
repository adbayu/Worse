//! Remote Control through the relay (docs/plans/remote-relay.md), app side: runs on the remote server thread
//! next to the loopback server, publishes the status, the link and the QR link into the page snapshot.

use std::sync::Mutex;

use harness_remote::{RelayHandle, RelayIdentity, RelayStatus, RemoteHandle};
use tokio::sync::watch;

use super::snapshot;

/// Status receiver of the running relay connection (read by the UI timer).
static STATUS: Mutex<Option<watch::Receiver<RelayStatus>>> = Mutex::new(None);

fn status_slot() -> std::sync::MutexGuard<'static, Option<watch::Receiver<RelayStatus>>> {
    match STATUS.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Current relay status for the page.
pub(super) fn status() -> RelayStatus {
    status_slot()
        .as_ref()
        .map_or(RelayStatus::Off, |rx| rx.borrow().clone())
}

/// Name phones show: the setting, else the computer's host name.
pub(crate) fn device_name(setting: &str) -> String {
    let name = setting.trim();
    if !name.is_empty() {
        return name.to_owned();
    }
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|v| !v.trim().is_empty()))
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "desktop".to_owned())
}

/// Percent-encodes a fragment value (`A-Za-z0-9-._~` stay).
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The QR link: the plain link plus a single-use pair offer and the device name.
pub(super) fn qr_link(link: &str, pair_token: &str, name: &str) -> String {
    if link.is_empty() {
        return String::new();
    }
    format!("{link}&p={pair_token}&n={}", encode(name))
}

/// Connects (or reconnects) the running server to `url`; an empty URL only stops the relay.
pub(super) fn start(
    runtime: &tokio::runtime::Runtime,
    server: &RemoteHandle,
    relay: &mut Option<RelayHandle>,
    url: &str,
) {
    stop(runtime, relay);
    if url.trim().is_empty() {
        return;
    }
    // 修复：start_relay 内部 tokio::spawn，必须在运行时上下文里调用，否则 panic 并杀死整个远程线程。
    let _runtime = runtime.enter();
    match server.start_relay(url) {
        Ok(handle) => {
            *status_slot() = Some(handle.subscribe());
            *relay = Some(handle);
            publish_link(server, url);
        }
        Err(error) => {
            let (tx, rx) = watch::channel(RelayStatus::Error {
                message: error.to_string(),
            });
            drop(tx);
            *status_slot() = Some(rx);
        }
    }
}

pub(super) fn stop(runtime: &tokio::runtime::Runtime, relay: &mut Option<RelayHandle>) {
    if let Some(running) = relay.take() {
        runtime.block_on(running.stop());
    }
    *status_slot() = None;
    let mut state = snapshot();
    state.relay_link = String::new();
    state.relay_qr = String::new();
    state.relay_code = String::new();
}

fn publish_link(server: &RemoteHandle, url: &str) {
    let link = RelayIdentity::load_or_create(&server.state_dir())
        .map(|identity| identity.link(url))
        .unwrap_or_default();
    let mut state = snapshot();
    state.relay_link = link;
    state.relay_qr = String::new();
    state.relay_code = String::new();
}

/// A fresh QR link (new single-use pair offer); called when the QR is shown.
pub(super) fn new_qr(server: &RemoteHandle, name: &str) {
    // 中继未连接时没有链接，不要白白生成一个可用的配对令牌。
    if snapshot().relay_link.is_empty() {
        return;
    }
    let offer = server.new_pair_offer("Phone");
    let mut state = snapshot();
    state.relay_qr = qr_link(&state.relay_link, &offer.token, name);
    // 修复：只打开链接（没有 QR 里的 p=）的手机需要输入配对码，桌面端必须显示它。
    state.relay_code = offer.code;
}

/// New link secret: phones paired through the old link must scan again.
pub(super) fn reset_link(
    runtime: &tokio::runtime::Runtime,
    server: &RemoteHandle,
    relay: &mut Option<RelayHandle>,
    url: &str,
) {
    if let Err(error) = RelayIdentity::reset_secret(&server.state_dir()) {
        snapshot().note = format!("cannot reset the link: {error}");
        return;
    }
    // 安全修复：重置链接必须同时注销所有已配对设备，否则旧设备令牌仍然有效。
    for device in server.devices() {
        server.revoke(&device.id);
    }
    // Connected phones hold the old secret; reconnecting drops them.
    start(runtime, server, relay, url);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_link_carries_offer_and_name() {
        assert_eq!(
            qr_link("https://r.example/abc#k=K", "T0", "my pc/1"),
            "https://r.example/abc#k=K&p=T0&n=my%20pc%2F1"
        );
        assert_eq!(qr_link("", "T0", "x"), "");
        assert_eq!(device_name(" Studio "), "Studio");
        assert_ne!(device_name(""), "");
    }
}
