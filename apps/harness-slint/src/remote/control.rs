use super::{
    send, set_server, set_tunnel, snapshot, tunnel, Cmd, RemoteConfig, ServerState, STATE_DIR,
    TUNNEL,
};

/// Opens an `https://` link in the default browser.
pub(crate) fn open_url(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    let program = if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(url).spawn();
}

/// Starts the local server (page toggle switched on, or "Start server").
pub fn start_server() {
    let Some(settings) = crate::settings::current() else {
        set_server(ServerState::Error {
            message: "settings are not loaded yet".into(),
        });
        return;
    };
    let Some(core) = crate::core::hub::core_handle() else {
        set_server(ServerState::Error {
            message: "the agent hub is still starting; enable the server again in a moment".into(),
        });
        return;
    };
    let state_dir = STATE_DIR.get().cloned().unwrap_or_default();
    snapshot().port = settings.remote.port;
    send(Cmd::Start {
        cfg: RemoteConfig {
            port: settings.remote.port,
            state_dir,
        },
        core,
    });
    send(Cmd::Relay(settings.remote.relay_url.clone()));
}

/// Starts the configured tunnel for the running server.
pub fn start_tunnel() {
    let Some(settings) = crate::settings::current() else {
        return;
    };
    let port = if let ServerState::Up { .. } = snapshot().server.clone() {
        settings.remote.port
    } else {
        snapshot().note = "Start the server before the tunnel.".into();
        return;
    };
    let cfg = tunnel::TunnelConfig::new(
        &settings.remote.tunnel_bin,
        port,
        &settings.remote.subdomain,
        &settings.remote.tunnel_domain,
    );
    let tunnel = tunnel_or_new();
    let state = match tunnel.start(&cfg) {
        Ok(()) => tunnel.state(),
        Err(message) => tunnel::State::Error { message },
    };
    snapshot().note = String::new();
    set_tunnel(state);
}

/// Stops the tunnel (page button, or the server being switched off).
pub fn stop_tunnel() {
    if let Some(tunnel) = tunnel_handle() {
        tunnel.stop();
    }
    send(Cmd::PublicHost(None));
    let mut state = snapshot();
    state.tunnel = tunnel::State::Off;
    state.public_url = String::new();
    state.offer_url = String::new();
    state.offer_code = String::new();
    state.offer_expires = 0;
}

/// The one tunnel supervisor of the process (lazily created).
pub(super) fn tunnel_handle() -> Option<tunnel::Tunnel> {
    let slot = match TUNNEL.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    slot.as_ref().cloned()
}

/// The tunnel supervisor, created on first use.
fn tunnel_or_new() -> tunnel::Tunnel {
    let mut slot = match TUNNEL.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    slot.get_or_insert_with(tunnel::Tunnel::new).clone()
}
