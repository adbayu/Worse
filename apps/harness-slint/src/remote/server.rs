use super::{
    mpsc, relay, send, settings_view, snapshot, tunnel, tunnel_handle, Cmd, RemoteHandle,
    ServerState,
};

/// Server thread: one runtime, one handle, commands applied in order.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn serve(rx: mpsc::Receiver<Cmd>) {
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("harness-remote-io")
        .enable_all()
        .build()
    else {
        set_server(ServerState::Error {
            message: "cannot start the remote server runtime".into(),
        });
        return;
    };
    let mut handle: Option<RemoteHandle> = None;
    let mut relay_conn = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Start { cfg, core } => {
                if handle.is_some() {
                    continue;
                }
                let port = cfg.port;
                snapshot().port = port;
                set_server(ServerState::Starting);
                match runtime.block_on(RemoteHandle::start(cfg, core)) {
                    Ok(started) => {
                        let addr = started.local_addr().to_string();
                        snapshot().devices = started.devices();
                        started.set_public_host(host_of(&snapshot().public_url));
                        started
                            .set_settings_view(std::sync::Arc::new(settings_view::DesktopSettings));
                        handle = Some(started);
                        set_server(ServerState::Up { addr });
                    }
                    Err(error) => set_server(ServerState::Error {
                        message: format!("cannot serve on 127.0.0.1:{port}: {error}"),
                    }),
                }
            }
            Cmd::Stop => {
                relay::stop(&runtime, &mut relay_conn);
                if let Some(running) = handle.take() {
                    runtime.block_on(running.shutdown());
                }
                set_server(ServerState::Off);
            }
            Cmd::Pair { name } => {
                if let Some(running) = &handle {
                    let offer = running.new_pair_offer(&name);
                    let mut state = snapshot();
                    state.offer_url = pair_url(&state.public_url, &offer.token);
                    state.offer_code = offer.code;
                    state.offer_expires = offer.expires_ms;
                    state.note = String::new();
                }
            }
            Cmd::Devices => {
                if let Some(running) = &handle {
                    snapshot().devices = running.devices();
                }
            }
            Cmd::Revoke(id) => {
                if let Some(running) = &handle {
                    running.revoke(&id);
                    snapshot().devices = running.devices();
                }
            }
            Cmd::PublicHost(host) => {
                if let Some(running) = &handle {
                    running.set_public_host(host);
                }
            }
            Cmd::Relay(url) => {
                if let Some(running) = &handle {
                    relay::start(&runtime, running, &mut relay_conn, &url);
                }
            }
            Cmd::RelayQr(name) => {
                if let Some(running) = &handle {
                    relay::new_qr(running, &name);
                }
            }
            Cmd::ResetLink(url) => {
                if let Some(running) = &handle {
                    relay::reset_link(&runtime, running, &mut relay_conn, &url);
                }
            }
        }
    }
    relay::stop(&runtime, &mut relay_conn);
    if let Some(running) = handle.take() {
        runtime.block_on(running.shutdown());
    }
    set_server(ServerState::Off);
}

pub(super) fn set_server(state: ServerState) {
    snapshot().server = state;
}

pub(super) fn set_tunnel(state: tunnel::State) {
    snapshot().tunnel = state;
}

/// `https://<host>/p#t=<token>` (docs/plans/remote-contract.md "Pair link").
pub(super) fn pair_url(public_url: &str, token: &str) -> String {
    let base = public_url.trim().trim_end_matches('/');
    if base.is_empty() {
        String::new()
    } else {
        format!("{base}/p#t={token}")
    }
}

/// Host of `https://host/…`, the form `RemoteHandle::set_public_host` expects.
pub(super) fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest
        .split(['/', '#', '?'])
        .next()
        .unwrap_or_default()
        .trim();
    (!host.is_empty()).then(|| host.to_owned())
}

/// Copies the tunnel's live state into the snapshot (UI timer, before painting).
pub(super) fn poll_tunnel() {
    let Some(tunnel) = tunnel_handle() else {
        return;
    };
    let state = tunnel.state();
    let log = tunnel.log();
    let mut state_snapshot = snapshot();
    state_snapshot.tunnel = state;
    state_snapshot.log = log;
}

/// Keeps the server's accepted `Host` in step with the tunnel state.
pub(super) fn sync_public_host() {
    let (tunnel_state, current) = {
        let state = snapshot();
        (state.tunnel.clone(), state.public_url.clone())
    };
    let next = match &tunnel_state {
        tunnel::State::Up { url } => url.clone(),
        _ => String::new(),
    };
    if next == current {
        return;
    }
    send(Cmd::PublicHost(host_of(&next)));
    let mut state = snapshot();
    state.public_url = next;
    // A QR made for the old host would pair against a dead URL.
    state.offer_url = String::new();
    state.offer_code = String::new();
    state.offer_expires = 0;
}
