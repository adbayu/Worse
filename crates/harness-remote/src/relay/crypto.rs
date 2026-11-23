//! End-to-end encryption between a phone and this desktop (docs/plans/remote-relay.md §3): per connection
//! keys from HKDF-SHA256 over the link secret and both salts, AES-256-GCM frames with a strictly increasing
//! sequence number, and chunking of large messages.

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::hkdf::{Salt, HKDF_SHA256};

pub(crate) const HELLO: u8 = 1;
const DATA: u8 = 2;
pub(crate) const SALT_LEN: usize = 16;
/// Plaintext bytes per frame (relay frames stay well under its 1 MiB cap).
pub(crate) const CHUNK: usize = 256 * 1024;
/// Largest joined message.
pub(crate) const MAX_MESSAGE: usize = 32 * 1024 * 1024;
const MORE: u8 = 1;

/// Keys of one phone connection, seen from one side.
pub(crate) struct Keys {
    send: LessSafeKey,
    recv: LessSafeKey,
    send_seq: u64,
    recv_seq: u64,
}

fn derive(secret: &[u8; 32], salt: &[u8], info: &str) -> LessSafeKey {
    let prk = Salt::new(HKDF_SHA256, salt).extract(secret);
    let info = [info.as_bytes()];
    // 32 bytes from HKDF-SHA256 are always available (limit 255 * 32).
    let okm = prk
        .expand(&info, &AES_256_GCM)
        .unwrap_or_else(|_| unreachable!("HKDF output length is valid"));
    LessSafeKey::new(UnboundKey::from(okm))
}

fn header(seq: u64) -> [u8; 9] {
    let mut head = [0_u8; 9];
    head[0] = DATA;
    head[1..].copy_from_slice(&seq.to_be_bytes());
    head
}

fn nonce(seq: u64) -> Nonce {
    let mut bytes = [0_u8; 12];
    bytes[4..].copy_from_slice(&seq.to_be_bytes());
    Nonce::assume_unique_for_key(bytes)
}

impl Keys {
    fn new(secret: &[u8; 32], salt_c: &[u8], salt_h: &[u8], host: bool) -> Self {
        let salt = [salt_c, salt_h].concat();
        let c2h = derive(secret, &salt, "worse-relay/1 c2h");
        let h2c = derive(secret, &salt, "worse-relay/1 h2c");
        let (send, recv) = if host { (h2c, c2h) } else { (c2h, h2c) };
        Self {
            send,
            recv,
            send_seq: 0,
            recv_seq: 0,
        }
    }

    pub(crate) fn host(secret: &[u8; 32], salt_c: &[u8], salt_h: &[u8]) -> Self {
        Self::new(secret, salt_c, salt_h, true)
    }

    #[cfg(test)]
    pub(crate) fn phone(secret: &[u8; 32], salt_c: &[u8], salt_h: &[u8]) -> Self {
        Self::new(secret, salt_c, salt_h, false)
    }

    /// One data frame for `plain` (flags byte included by the caller).
    pub(crate) fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        let seq = self.send_seq;
        self.send_seq += 1;
        let head = header(seq);
        let mut body = plain.to_vec();
        // AES-GCM sealing only fails for inputs over ~64 GiB.
        if self
            .send
            .seal_in_place_append_tag(nonce(seq), Aad::from(head), &mut body)
            .is_err()
        {
            body.clear();
        }
        [head.as_slice(), &body].concat()
    }

    /// Plaintext of the next frame; `None` for a forged, replayed or reordered frame.
    pub(crate) fn open(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
        if frame.len() < 9 + 16 || frame[0] != DATA {
            return None;
        }
        let seq = u64::from_be_bytes(frame[1..9].try_into().ok()?);
        if seq != self.recv_seq {
            return None;
        }
        let mut body = frame[9..].to_vec();
        let plain = self
            .recv
            .open_in_place(nonce(seq), Aad::from(header(seq)), &mut body)
            .ok()?
            .to_vec();
        self.recv_seq += 1;
        Some(plain)
    }

    /// Frames for one whole message (chunked, each with its flags byte).
    pub(crate) fn seal_message(&mut self, message: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let mut parts = message.chunks(CHUNK).peekable();
        if parts.peek().is_none() {
            return vec![self.seal(&[0])];
        }
        while let Some(part) = parts.next() {
            let flags = if parts.peek().is_some() { MORE } else { 0 };
            frames.push(self.seal(&[&[flags], part].concat()));
        }
        frames
    }
}

/// Joins chunked plaintexts into messages.
#[derive(Default)]
pub(crate) struct Assembler {
    buf: Vec<u8>,
}

impl Assembler {
    /// `Ok(Some(message))` when a message is complete; `Err` when it grows past [`MAX_MESSAGE`].
    pub(crate) fn push(&mut self, plain: &[u8]) -> Result<Option<Vec<u8>>, ()> {
        let (&flags, chunk) = plain.split_first().ok_or(())?;
        if self.buf.len() + chunk.len() > MAX_MESSAGE {
            return Err(());
        }
        self.buf.extend_from_slice(chunk);
        Ok((flags & MORE == 0).then(|| std::mem::take(&mut self.buf)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range<const N: usize>(start: u8) -> [u8; N] {
        std::array::from_fn(|i| start + u8::try_from(i).unwrap())
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    /// Vectors from `WebCrypto` (the phone client's implementation), so both sides agree byte for byte.
    #[test]
    fn matches_webcrypto_vectors() {
        let (secret, salt_c, salt_h) = (range::<32>(0), range::<16>(32), range::<16>(48));
        let mut phone = Keys::phone(&secret, &salt_c, &salt_h);
        let mut host = Keys::host(&secret, &salt_c, &salt_h);
        let plain = b"\0hello";
        let first = phone.seal(plain);
        assert_eq!(
            hex(&first),
            "02000000000000000084458669264a93811c3ce0ccc5a1a3629daee512144f"
        );
        assert_eq!(
            hex(&phone.seal(plain)),
            "0200000000000000016d7969532f5ab65ac9861173e6859cebb339c3c8a725"
        );
        assert_eq!(
            hex(&host.seal(plain)),
            "02000000000000000017ccc819c63375d3fc1dca464d28f4fba39c3b2246d6"
        );
        assert_eq!(host.open(&first).as_deref(), Some(plain.as_slice()));
    }

    #[test]
    fn rejects_replay_tamper_and_reorder() {
        let secret = [7_u8; 32];
        let mut phone = Keys::phone(&secret, &[1; 16], &[2; 16]);
        let mut host = Keys::host(&secret, &[1; 16], &[2; 16]);
        let a = phone.seal(b"\0a");
        let b = phone.seal(b"\0b");
        assert!(host.open(&b).is_none(), "out of order");
        assert!(host.open(&a).is_some());
        assert!(host.open(&a).is_none(), "replay");
        let mut forged = b.clone();
        let last = forged.len() - 1;
        forged[last] ^= 1;
        assert!(host.open(&forged).is_none(), "tampered");
        assert!(host.open(&b).is_some());
        let mut other = Keys::host(&secret, &[1; 16], &[3; 16]);
        assert!(other.open(&phone.seal(b"\0c")).is_none(), "other salts");
    }

    #[test]
    fn chunks_join_back() {
        let secret = [9_u8; 32];
        let mut phone = Keys::phone(&secret, &[1; 16], &[2; 16]);
        let mut host = Keys::host(&secret, &[1; 16], &[2; 16]);
        let message: Vec<u8> = (0..CHUNK * 2 + 10)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let frames = phone.seal_message(&message);
        assert_eq!(frames.len(), 3);
        let mut asm = Assembler::default();
        let mut out = None;
        for frame in frames {
            out = asm.push(&host.open(&frame).unwrap()).unwrap();
        }
        assert_eq!(out.unwrap(), message);
        assert_eq!(
            asm.push(&host.open(&phone.seal_message(b"")[0]).unwrap()),
            Ok(Some(Vec::new()))
        );
    }
}
