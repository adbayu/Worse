//! Device tokens, single-use pair offers and per-IP rate limiting.
//!
//! Devices live in `<state_dir>/remote.json`, written atomically (tmp file + rename). Tokens are
//! compared in constant time; nothing in this module logs a token or a pair code.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;

use crate::{DeviceInfo, PairOffer};

/// Paired device cap (spec §3.3).
const MAX_DEVICES: usize = 8;
/// Pair offers live 10 minutes.
const OFFER_TTL_MS: u64 = 10 * 60 * 1_000;
/// Failed pair/auth attempts per IP…
const RATE_LIMIT: u32 = 10;
/// …inside this window.
const RATE_WINDOW_MS: u64 = 60 * 1_000;
/// Unambiguous short-code alphabet: `A-Z` without `0`, `1`, `I`, `O`.
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// One paired device (the on-disk row).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    id: String,
    name: String,
    /// 64 hex chars; the cookie value.
    token: String,
    created_ms: u64,
    last_seen_ms: u64,
}

impl Device {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            created_ms: self.created_ms,
            last_seen_ms: self.last_seen_ms,
        }
    }
}

/// A pending pair offer (memory only, never written to disk).
#[derive(Debug, Clone)]
struct Offer {
    token: String,
    code: String,
    name: String,
    expires_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredFile {
    #[serde(default)]
    devices: Vec<Device>,
}

#[derive(Debug, Default)]
struct State {
    devices: Vec<Device>,
    offers: Vec<Offer>,
}

#[derive(Debug, Clone, Copy)]
struct Window {
    start_ms: u64,
    failures: u32,
}

/// Token store + rate limiter.
pub(crate) struct Auth {
    file: PathBuf,
    state: Mutex<State>,
    rates: Mutex<HashMap<IpAddr, Window>>,
    revoked: broadcast::Sender<String>,
}

impl Auth {
    /// Loads `state_dir/remote.json`; a missing or unreadable file starts empty (never fails).
    pub(crate) fn load(state_dir: &Path) -> Self {
        let file = state_dir.join("remote.json");
        let state = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<StoredFile>(&bytes).ok())
            .map_or_else(State::default, |stored| State {
                devices: stored.devices,
                offers: Vec::new(),
            });
        let (revoked, _) = broadcast::channel(16);
        Self {
            file,
            state: Mutex::new(state),
            rates: Mutex::new(HashMap::new()),
            revoked,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn rates(&self) -> MutexGuard<'_, HashMap<IpAddr, Window>> {
        self.rates.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn devices(&self) -> Vec<DeviceInfo> {
        self.lock().devices.iter().map(Device::info).collect()
    }

    /// Creates a fresh single-use offer (token for the QR/link, code for typing).
    pub(crate) fn new_offer(&self, device_name: &str) -> PairOffer {
        let now = now_ms();
        let mut state = self.lock();
        state.offers.retain(|offer| offer.expires_ms > now);
        let token = hex(&random_bytes(32));
        let code = loop {
            let candidate = short_code();
            if !state.offers.iter().any(|offer| offer.code == candidate) {
                break candidate;
            }
        };
        let expires_ms = now + OFFER_TTL_MS;
        state.offers.push(Offer {
            token: token.clone(),
            code: code.clone(),
            name: device_name.trim().to_owned(),
            expires_ms,
        });
        PairOffer {
            token,
            code,
            expires_ms,
        }
    }

    /// Redeems a pair offer by token or by short code. Single use; `None` when it is unknown,
    /// expired or already used. At the device cap the least recently seen device is unpaired.
    pub(crate) fn pair(
        &self,
        token: Option<&str>,
        code: Option<&str>,
        name: Option<&str>,
    ) -> Option<(String, String)> {
        let now = now_ms();
        let mut state = self.lock();
        state.offers.retain(|offer| offer.expires_ms > now);
        let index = state.offers.iter().position(|offer| match (token, code) {
            (Some(token), _) if !token.is_empty() => ct_eq(&offer.token, token),
            (_, Some(code)) if !code.is_empty() => {
                ct_eq(&offer.code, &code.trim().to_ascii_uppercase())
            }
            _ => false,
        })?;
        let offer = state.offers.remove(index);
        while state.devices.len() >= MAX_DEVICES {
            let Some(oldest) = state
                .devices
                .iter()
                .enumerate()
                .min_by_key(|(_, device)| device.last_seen_ms)
                .map(|(index, _)| index)
            else {
                break;
            };
            state.devices.remove(oldest);
        }
        let device = Device {
            id: hex(&random_bytes(16)),
            name: name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(&offer.name)
                .to_owned(),
            token: offer.token,
            created_ms: now,
            last_seen_ms: now,
        };
        let pair = (device.id.clone(), device.token.clone());
        state.devices.push(device);
        self.save(&state);
        Some(pair)
    }

    /// Device id for a cookie / bearer token; unknown tokens give `None`.
    pub(crate) fn device_for_token(&self, token: &str) -> Option<String> {
        if token.is_empty() {
            return None;
        }
        let now = now_ms();
        let mut state = self.lock();
        let index = state.devices.iter().position(|d| ct_eq(&d.token, token))?;
        let id = state.devices[index].id.clone();
        // Persist `last_seen_ms` at most once a minute per device.
        if now.saturating_sub(state.devices[index].last_seen_ms) > 60_000 {
            state.devices[index].last_seen_ms = now;
            self.save(&state);
        }
        Some(id)
    }

    /// Drops a device and tells every connection of it to close.
    pub(crate) fn revoke(&self, id: &str) {
        let mut state = self.lock();
        let before = state.devices.len();
        state.devices.retain(|device| device.id != id);
        if state.devices.len() != before {
            self.save(&state);
            let _ = self.revoked.send(id.to_owned());
        }
    }

    pub(crate) fn revoked_rx(&self) -> broadcast::Receiver<String> {
        self.revoked.subscribe()
    }

    /// Test helper: marks the offer with `token` as expired.
    #[cfg(test)]
    pub(crate) fn expire(&self, token: &str) {
        let mut state = self.lock();
        for offer in &mut state.offers {
            if offer.token == token {
                offer.expires_ms = 0;
            }
        }
    }

    /// True while `ip` is under the failed-attempt limit (checked before verifying credentials).
    pub(crate) fn allow(&self, ip: IpAddr) -> bool {
        let now = now_ms();
        let mut rates = self.rates();
        let window = rates.entry(ip).or_insert(Window {
            start_ms: now,
            failures: 0,
        });
        if now.saturating_sub(window.start_ms) >= RATE_WINDOW_MS {
            *window = Window {
                start_ms: now,
                failures: 0,
            };
        }
        window.failures < RATE_LIMIT
    }

    /// Records a failed attempt; returns false once the IP is over the limit (→ 429).
    pub(crate) fn fail(&self, ip: IpAddr) -> bool {
        let now = now_ms();
        let mut rates = self.rates();
        let window = rates.entry(ip).or_insert(Window {
            start_ms: now,
            failures: 0,
        });
        if now.saturating_sub(window.start_ms) >= RATE_WINDOW_MS {
            *window = Window {
                start_ms: now,
                failures: 0,
            };
        }
        window.failures = window.failures.saturating_add(1);
        window.failures <= RATE_LIMIT
    }

    fn save(&self, state: &State) {
        let stored = StoredFile {
            devices: state.devices.clone(),
        };
        let Ok(json) = serde_json::to_vec_pretty(&stored) else {
            return;
        };
        if let Some(parent) = self.file.parent() {
            if !parent.as_os_str().is_empty() && std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let tmp = self.file.with_extension("json.tmp");
        if std::fs::write(&tmp, &json).is_err() {
            return;
        }
        if std::fs::rename(&tmp, &self.file).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// Constant-time equality of two strings (length leaks only).
pub(crate) fn ct_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// `n` random bytes from the OS RNG, with a time-seeded fallback when the OS RNG fails.
fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0_u8; n];
    if getrandom::fill(&mut buf).is_ok() {
        return buf;
    }
    let seed = now_ms()
        .rotate_left(17)
        .wrapping_add(u64::from(std::process::id()))
        | 1;
    let mut state = seed;
    for byte in &mut buf {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = u8::try_from((state >> 24) & 0xFF).unwrap_or(0);
    }
    buf
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// 8 chars of [`CODE_ALPHABET`] (uniform: the alphabet is a power of two).
fn short_code() -> String {
    random_bytes(8)
        .into_iter()
        .map(|byte| char::from(CODE_ALPHABET[usize::from(byte & 31)]))
        .collect()
}

#[cfg(test)]
mod tests;
