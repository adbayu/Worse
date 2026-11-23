//! Paints the "Remote Control" card (relay status, link, QR) and restarts Remote Control at launch when it was
//! left on. UI thread, called from the page timer.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use harness_remote::RelayStatus;
use slint::SharedString;

use super::{qr, relay, send, snapshot, Cmd, RemoteUi, ServerState};

/// Pair offers live 10 minutes; the QR gets a new one a little before that.
const QR_REFRESH: Duration = Duration::from_secs(9 * 60);
/// Wait between two requests for a QR link that has not arrived yet.
const QR_RETRY: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Cache {
    qr: String,
    requested: Option<Instant>,
    auto_started: bool,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

pub(super) fn paint(ui: &RemoteUi<'_>, settings: Option<&harness_settings::Settings>) {
    let status = relay::status();
    ui.set_relay_state(match &status {
        RelayStatus::Off => 0,
        RelayStatus::Connecting => 1,
        RelayStatus::Online { .. } => 2,
        RelayStatus::Error { .. } => 3,
    });
    ui.set_relay_text(SharedString::from(match &status {
        RelayStatus::Off => "Off".to_owned(),
        RelayStatus::Connecting => "Connecting to the relay…".to_owned(),
        RelayStatus::Online { phones: 0 } => "Connected".to_owned(),
        RelayStatus::Online { phones: 1 } => "Connected · 1 device open".to_owned(),
        RelayStatus::Online { phones } => format!("Connected · {phones} devices open"),
        RelayStatus::Error { message } => message.clone(),
    }));
    ui.set_device_name_default(SharedString::from(relay::device_name("")));
    let (server, link, qr_link, code) = {
        let state = snapshot();
        (
            state.server.clone(),
            state.relay_link.clone(),
            state.relay_qr.clone(),
            state.relay_code.clone(),
        )
    };
    ui.set_relay_link(SharedString::from(link.as_str()));
    ui.set_relay_code(SharedString::from(code.as_str()));
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.qr != qr_link {
            let image = (!qr_link.is_empty()).then(|| qr::image(&qr_link)).flatten();
            ui.set_has_relay_qr(image.is_some());
            if let Some(image) = image {
                ui.set_relay_qr(image);
            }
            cache.qr.clone_from(&qr_link);
        }
        let due = match cache.requested {
            None => true,
            Some(at) if qr_link.is_empty() => at.elapsed() >= QR_RETRY,
            Some(at) => at.elapsed() >= QR_REFRESH,
        };
        if !link.is_empty() && due {
            let name = settings
                .map(|s| s.remote.device_name.as_str())
                .unwrap_or_default();
            send(Cmd::RelayQr(relay::device_name(name)));
            cache.requested = Some(Instant::now());
        }
        if link.is_empty() {
            cache.requested = None;
        }
        // Remote Control was left on: start it again once the hub can serve it.
        let wanted = settings.is_some_and(|s| s.remote.enabled);
        if wanted
            && !cache.auto_started
            && server == ServerState::Off
            && crate::core::hub::core_handle().is_some()
        {
            cache.auto_started = true;
            super::start_server();
        }
    });
}
