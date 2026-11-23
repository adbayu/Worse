//! The desktop's relay identity (docs/plans/remote-relay.md §1): an Ed25519 key (its hash is the device id)
//! and the link secret `k`, kept in `<state_dir>/relay.json` (tmp file + rename, never logged).

use std::io;
use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use serde::{Deserialize, Serialize};

const FILE: &str = "relay.json";
const ID_LEN: usize = 16;
const B32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

#[derive(Serialize, Deserialize)]
struct Stored {
    seed: String,
    secret: String,
}

/// Key pair and link secret of this computer.
pub struct RelayIdentity {
    seed: [u8; 32],
    secret: [u8; 32],
}

impl std::fmt::Debug for RelayIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayIdentity")
            .field("device_id", &self.device_id())
            .finish_non_exhaustive()
    }
}

fn random32() -> io::Result<[u8; 32]> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(bytes)
}

fn decode32(text: &str) -> Option<[u8; 32]> {
    URL_SAFE_NO_PAD.decode(text).ok()?.try_into().ok()
}

pub(crate) fn base32(bytes: &[u8]) -> String {
    let mut out = String::new();
    let (mut value, mut bits) = (0_u32, 0_u32);
    for &byte in bytes {
        value = (value << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            out.push(char::from(B32[((value >> (bits - 5)) & 31) as usize]));
            bits -= 5;
        }
        value &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(B32[((value << (5 - bits)) & 31) as usize]));
    }
    out
}

/// `https://relay.example` → base without a trailing slash; only `https://`, or `http://` on loopback (tests).
pub(crate) fn relay_base(url: &str) -> Option<&str> {
    let base = url.trim().trim_end_matches('/');
    // 安全修复：按主机名精确判断，`http://127.0.0.1.evil.com` 之类的前缀不再算作本机明文地址。
    let (scheme, rest) = base.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(authority, |(host, _)| host);
    let ok = match scheme {
        "https" => true,
        "http" => host == "127.0.0.1" || host == "localhost",
        _ => false,
    };
    ok.then_some(base)
}

impl RelayIdentity {
    /// Loads `relay.json`, creating it with a new key and secret on first use.
    ///
    /// # Errors
    /// The file cannot be read, parsed or written, or no randomness is available.
    pub fn load_or_create(state_dir: &Path) -> io::Result<Self> {
        let file = state_dir.join(FILE);
        match std::fs::read(&file) {
            Ok(bytes) => {
                let stored: Stored = serde_json::from_slice(&bytes)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                match (decode32(&stored.seed), decode32(&stored.secret)) {
                    (Some(seed), Some(secret)) => Ok(Self { seed, secret }),
                    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "bad relay.json")),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let identity = Self {
                    seed: random32()?,
                    secret: random32()?,
                };
                identity.save(state_dir)?;
                Ok(identity)
            }
            Err(e) => Err(e),
        }
    }

    /// New link secret (same device id): every phone has to scan the QR code again.
    ///
    /// # Errors
    /// See [`Self::load_or_create`].
    pub fn reset_secret(state_dir: &Path) -> io::Result<Self> {
        let mut identity = Self::load_or_create(state_dir)?;
        identity.secret = random32()?;
        identity.save(state_dir)?;
        Ok(identity)
    }

    fn save(&self, state_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        let json = serde_json::to_vec_pretty(&Stored {
            seed: URL_SAFE_NO_PAD.encode(self.seed),
            secret: URL_SAFE_NO_PAD.encode(self.secret),
        })
        .map_err(|e| io::Error::other(e.to_string()))?;
        let file = state_dir.join(FILE);
        let tmp = state_dir.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &file)
    }

    fn key_pair(&self) -> Ed25519KeyPair {
        // 32-byte seed is always a valid Ed25519 private key.
        match Ed25519KeyPair::from_seed_unchecked(&self.seed) {
            Ok(pair) => pair,
            Err(_) => unreachable!("a 32-byte seed is a valid Ed25519 key"),
        }
    }

    /// 16 chars of base32(SHA-256(public key)).
    #[must_use]
    pub fn device_id(&self) -> String {
        let pair = self.key_pair();
        let hash = ring::digest::digest(&ring::digest::SHA256, pair.public_key().as_ref());
        base32(hash.as_ref())[..ID_LEN].to_owned()
    }

    /// The link a phone opens: `<relay>/<id>#k=<secret>` (the fragment never reaches the relay).
    #[must_use]
    pub fn link(&self, relay_url: &str) -> String {
        let base = relay_base(relay_url).unwrap_or(relay_url);
        format!(
            "{base}/{}#k={}",
            self.device_id(),
            URL_SAFE_NO_PAD.encode(self.secret)
        )
    }

    pub(crate) fn secret(&self) -> &[u8; 32] {
        &self.secret
    }

    /// `wss://…/<id>/host?pub=&ts=&sig=` signed for `now_s` (unix seconds).
    pub(crate) fn host_url(&self, relay_url: &str, now_s: u64) -> Option<String> {
        let base = relay_base(relay_url)?;
        let ws = base
            .strip_prefix("https://")
            .map(|rest| format!("wss://{rest}"))
            .or_else(|| {
                base.strip_prefix("http://")
                    .map(|rest| format!("ws://{rest}"))
            })?;
        let id = self.device_id();
        let pair = self.key_pair();
        let sig = pair.sign(format!("worse-relay/1 host {id} {now_s}").as_bytes());
        Some(format!(
            "{ws}/{id}/host?pub={}&ts={now_s}&sig={}",
            URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()),
            URL_SAFE_NO_PAD.encode(sig.as_ref())
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_matches_rfc4648() {
        assert_eq!(base32(b"foobar"), "mzxw6ytboi");
        assert_eq!(base32(b"f"), "my");
    }

    #[test]
    fn relay_base_allows_plain_http_only_on_loopback() {
        assert_eq!(relay_base("https://r.example/"), Some("https://r.example"));
        assert_eq!(
            relay_base("http://127.0.0.1:8787"),
            Some("http://127.0.0.1:8787")
        );
        assert_eq!(relay_base("http://localhost/"), Some("http://localhost"));
        assert_eq!(relay_base("http://127.0.0.1.evil.com"), None);
        assert_eq!(relay_base("http://localhost.evil.com:80"), None);
        assert_eq!(relay_base("http://127.0.0.1@evil.com"), None);
        assert_eq!(relay_base("http://r.example"), None);
        assert_eq!(relay_base("ws://r.example"), None);
    }

    #[test]
    fn identity_persists_and_signs() {
        let dir = std::env::temp_dir().join(format!("harness-relay-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = RelayIdentity::load_or_create(&dir).unwrap();
        let again = RelayIdentity::load_or_create(&dir).unwrap();
        let id = first.device_id();
        assert_eq!(id, again.device_id());
        assert_eq!(id.len(), 16);
        assert!(id.bytes().all(|b| B32.contains(&b)));
        assert!(first
            .link("https://relay.example/")
            .starts_with(&format!("https://relay.example/{id}#k=")));

        let url = first
            .host_url("https://relay.example", 1_700_000_000)
            .unwrap();
        assert!(url.starts_with(&format!("wss://relay.example/{id}/host?pub=")));
        let query: std::collections::HashMap<_, _> = url
            .split_once('?')
            .unwrap()
            .1
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .collect();
        let public = URL_SAFE_NO_PAD.decode(query["pub"]).unwrap();
        let sig = URL_SAFE_NO_PAD.decode(query["sig"]).unwrap();
        let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public);
        let message = format!("worse-relay/1 host {id} 1700000000");
        assert!(key.verify(message.as_bytes(), &sig).is_ok());

        let reset = RelayIdentity::reset_secret(&dir).unwrap();
        assert_eq!(reset.device_id(), id);
        assert_ne!(reset.secret(), first.secret());
        assert!(first.host_url("ftp://x", 0).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
