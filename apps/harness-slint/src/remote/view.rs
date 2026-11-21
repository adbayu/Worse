use super::{
    poll_tunnel, qr, send, settings_view, snapshot, sync_public_host, tunnel, AppWindow, Cmd,
    ComponentHandle, ModelRc, Rc, RefCell, RemoteDevice, RemoteUi, ServerState, SharedString,
    VecModel, DEVICE_TICKS,
};

#[derive(Default)]
struct Cache {
    offer_url: String,
    rendered: bool,
    log: Vec<String>,
    devices: Vec<(String, String, u64)>,
    ticks: u32,
}

thread_local! {
    static CACHE: RefCell<Cache> = const { RefCell::new(Cache::new()) };
}

impl Cache {
    const fn new() -> Self {
        Self {
            offer_url: String::new(),
            rendered: false,
            log: Vec::new(),
            devices: Vec::new(),
            ticks: 0,
        }
    }
}

/// Paints the page from the snapshot (UI thread, timer-driven).
#[allow(clippy::too_many_lines)]
pub(super) fn refresh(window: &AppWindow) {
    poll_tunnel();
    sync_public_host();
    let state = {
        let state = snapshot();
        (
            state.server.clone(),
            state.public_url.clone(),
            state.tunnel.clone(),
            state.log.clone(),
            state.offer_url.clone(),
            state.offer_code.clone(),
            state.offer_expires,
            state.devices.clone(),
            state.note.clone(),
        )
    };
    let (
        server,
        public_url,
        tunnel_state,
        log,
        offer_url,
        offer_code,
        offer_expires,
        devices,
        note,
    ) = state;
    let settings = crate::settings::current();
    settings_view::mirror(settings.clone());
    let ui = window.global::<RemoteUi>();
    ui.set_server_state(match &server {
        ServerState::Off => 0,
        ServerState::Starting => 1,
        ServerState::Up { .. } => 2,
        ServerState::Error { .. } => 3,
    });
    ui.set_server_text(sv(&match &server {
        ServerState::Off => "Off".to_owned(),
        ServerState::Starting => "Starting…".to_owned(),
        ServerState::Up { addr } => format!("Serving on 127.0.0.1:{}", port_of(addr)),
        ServerState::Error { message } => message.clone(),
    }));
    ui.set_has_url(!public_url.is_empty());
    ui.set_public_url(sv(&public_url));
    ui.set_local_url(sv(&match &server {
        ServerState::Up { addr } => format!("http://{addr}"),
        _ => String::new(),
    }));
    ui.set_tunnel_state(match &tunnel_state {
        tunnel::State::Off => 0,
        tunnel::State::Starting => 1,
        tunnel::State::Up { .. } => 2,
        tunnel::State::Error { .. } => 3,
    });
    ui.set_tunnel_text(sv(&match &tunnel_state {
        tunnel::State::Off => "Off".to_owned(),
        tunnel::State::Starting => "Waiting for the public URL…".to_owned(),
        tunnel::State::Up { url } => url.clone(),
        tunnel::State::Error { message } => message.clone(),
    }));
    ui.set_pair_url(sv(&offer_url));
    ui.set_pair_code(sv(&offer_code));
    ui.set_pair_expiry(sv(&expiry_text(offer_expires)));
    ui.set_note(sv(&note));
    if let Some(settings) = &settings {
        ui.set_tunnel_hint(sv(
            &match (&settings.remote.subdomain, &settings.remote.tunnel_domain) {
                (sub, _) if !sub.is_empty() => {
                    "Named tunnel: the host stays the same between starts.".to_owned()
                }
                (_, domain) if !domain.is_empty() => {
                    "Quick tunnel: a random host; the zone field is the fallback URL.".to_owned()
                }
                _ => "Quick tunnel: a new random host on every start, so re-pair by QR afterwards."
                    .to_owned(),
            },
        ));
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.log != log {
            let rows: Vec<SharedString> = log.iter().map(|line| sv(line)).collect();
            ui.set_tunnel_log(model(rows));
            cache.log = log;
        }
        if cache.offer_url != offer_url || !cache.rendered {
            let rendered = (!offer_url.is_empty())
                .then(|| qr::image(&offer_url))
                .flatten();
            let has_qr = rendered.is_some();
            ui.set_has_qr(has_qr);
            if let Some(image) = rendered {
                ui.set_qr(image);
            }
            cache.rendered = has_qr;
            cache.offer_url = offer_url;
        }
        let rows: Vec<(String, String, u64)> = devices
            .iter()
            .map(|d| (d.id.clone(), d.name.clone(), d.last_seen_ms))
            .collect();
        if rows != cache.devices {
            let list: Vec<RemoteDevice> = devices
                .iter()
                .map(|d| RemoteDevice {
                    id: sv(&d.id),
                    name: sv(if d.name.is_empty() {
                        "Phone"
                    } else {
                        d.name.as_str()
                    }),
                    created: sv(&crate::settings::date_of(
                        i64::try_from(d.created_ms).unwrap_or(0),
                    )),
                    last_seen: sv(&if d.last_seen_ms == 0 {
                        "never".to_owned()
                    } else {
                        crate::settings::date_of(i64::try_from(d.last_seen_ms).unwrap_or(0))
                    }),
                })
                .collect();
            ui.set_devices(model(list));
            cache.devices = rows;
        }
        cache.ticks = cache.ticks.wrapping_add(1);
        if cache.ticks % DEVICE_TICKS == 0 {
            send(Cmd::Devices);
        }
    });
}

/// Port of a `127.0.0.1:<port>` socket address.
pub(super) fn port_of(addr: &str) -> String {
    addr.rsplit(':').next().unwrap_or(addr).to_owned()
}

fn expiry_text(expires_ms: u64) -> String {
    if expires_ms == 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    if expires_ms <= now {
        return "expired".to_owned();
    }
    let minutes = (expires_ms - now).div_ceil(60_000);
    format!("valid for {minutes} min")
}

fn sv(text: &str) -> SharedString {
    SharedString::from(text)
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(items)))
}
