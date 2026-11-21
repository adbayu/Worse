//! Phone access: the `harness-remote` server plus the tunnel supervisor (docs/plans/remote-contract.md).
//!
//! The server runs on its own thread with a small multi-thread tokio runtime; the UI never awaits it. Commands go
//! through one channel, results land in a [`Snapshot`] that a Slint timer publishes to the "Remote (phone)"
//! settings page. Starting is user-driven: the page toggle starts the server, nothing runs at launch.

pub mod qr;
mod settings_view;
pub mod tunnel;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use harness_core_api::HarnessCore;
use harness_remote::{DeviceInfo, RemoteConfig, RemoteHandle};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;

use crate::ui::{AppWindow, RemoteDevice, RemoteUi};

mod control;
mod server;
mod view;

pub(crate) use control::open_url;
use control::tunnel_handle;
pub use control::{start_server, start_tunnel, stop_tunnel};
use server::{poll_tunnel, serve, set_server, set_tunnel, sync_public_host};
use view::refresh;

/// How often the timer republishes the page (ms).
const TICK_MS: u64 = 400;
/// Ticks between two device-list refreshes (~10 s).
const DEVICE_TICKS: u32 = 25;

/// State of the local HTTP/WS server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum ServerState {
    #[default]
    Off,
    Starting,
    Up {
        addr: String,
    },
    Error {
        message: String,
    },
}

/// Published state of the page (written by the server/tunnel threads, read by the UI timer).
struct Snapshot {
    server: ServerState,
    port: u16,
    /// Public URL the server currently accepts as `Host` ("" = loopback only).
    public_url: String,
    tunnel: tunnel::State,
    log: Vec<String>,
    offer_url: String,
    offer_code: String,
    offer_expires: u64,
    devices: Vec<DeviceInfo>,
    /// One-line page notice (a tunnel started before the server, for example).
    note: String,
}

static SNAPSHOT: Mutex<Snapshot> = Mutex::new(Snapshot {
    server: ServerState::Off,
    port: 0,
    public_url: String::new(),
    tunnel: tunnel::State::Off,
    log: Vec::new(),
    offer_url: String::new(),
    offer_code: String::new(),
    offer_expires: 0,
    devices: Vec::new(),
    note: String::new(),
});
static TUNNEL: Mutex<Option<tunnel::Tunnel>> = Mutex::new(None);
static COMMANDS: OnceLock<Sender<Cmd>> = OnceLock::new();
/// `HARNESS_HOME` (paired devices live in `remote.json` there).
static STATE_DIR: OnceLock<PathBuf> = OnceLock::new();

enum Cmd {
    Start {
        cfg: RemoteConfig,
        core: std::sync::Arc<dyn HarnessCore>,
    },
    Stop,
    Pair {
        name: String,
    },
    Devices,
    Revoke(String),
    PublicHost(Option<String>),
}

fn snapshot() -> MutexGuard<'static, Snapshot> {
    match SNAPSHOT.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn send(cmd: Cmd) {
    if let Some(tx) = COMMANDS.get() {
        let _ = tx.send(cmd);
    }
}

/// Installs the page callbacks and the state-publishing timer.
pub fn install(window: &AppWindow, home: &Path) {
    let _ = STATE_DIR.set(home.to_path_buf());
    let (tx, rx) = mpsc::channel();
    if COMMANDS.set(tx).is_ok() {
        std::thread::Builder::new()
            .name("harness-remote".into())
            .spawn(move || serve(rx))
            .ok();
    }
    wire(window);
    let weak = window.as_weak();
    let timer = Box::new(slint::Timer::default());
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(TICK_MS),
        move || {
            if let Some(window) = weak.upgrade() {
                refresh(&window);
            }
        },
    );
    // The timer must outlive this call; it shares the event loop with the window.
    Box::leak(timer);
}

/// Called when the page opens: publish at once and pull the device list.
pub fn open() {
    send(Cmd::Devices);
}

/// Stops the tunnel and the server; called on application exit.
pub fn shutdown() {
    stop_tunnel();
    send(Cmd::Stop);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if snapshot().server == ServerState::Off {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wire(window: &AppWindow) {
    let ui = window.global::<RemoteUi>();
    ui.on_set_enabled(|on| {
        crate::settings::patch_setting("remote.enabled", serde_json::Value::from(on));
        if on {
            start_server();
        } else {
            stop_tunnel();
            send(Cmd::Stop);
        }
    });
    ui.on_start_tunnel(start_tunnel);
    ui.on_stop_tunnel(stop_tunnel);
    ui.on_pair_new(|| {
        send(Cmd::Pair {
            name: "Phone".into(),
        });
    });
    ui.on_revoke(|id| send(Cmd::Revoke(id.to_string())));
    ui.on_refresh(|| send(Cmd::Devices));
    let weak = window.as_weak();
    ui.on_copy(move |text| {
        if let Some(window) = weak.upgrade() {
            window.invoke_copy_text(text);
        }
    });
    ui.on_open_url(|url| open_url(&url));
}

#[cfg(test)]
mod tests {
    use super::server::{host_of, pair_url};
    use super::view::port_of;

    #[test]
    fn pair_link_and_host() {
        assert_eq!(
            pair_url("https://harness.example.com/", "ab12"),
            "https://harness.example.com/p#t=ab12"
        );
        assert_eq!(pair_url("", "ab12"), "");
        assert_eq!(
            host_of("https://harness.example.com/p#t=x").as_deref(),
            Some("harness.example.com")
        );
        assert_eq!(host_of(""), None);
        assert_eq!(port_of("127.0.0.1:7878"), "7878");
    }
}
