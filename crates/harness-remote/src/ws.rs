//! WebSocket transport for `/api/v1/events`.
//!
//! RFC 6455 is hand-rolled (server side only, ~200 lines) so the crate adds no WS dependency:
//! the handshake is SHA-1 + base64 of the client key, and the frames are text frames from
//! [`harness_core_api::EventStream`]. Client frames are only read for `ping` and `close`.
//!
//! Protocol (contract): the first frame is `{"type":"hello","seq":N}`, then one
//! `{"type":"event","event":<HubEvent>}` per hub event; a lagging subscriber gets
//! `{"type":"resync"}` and the client re-opens its session.

use std::sync::Arc;

use base64::Engine;
use bytes::Bytes;
use http::header::{self, HeaderValue};
use http::{Response, StatusCode};
use http_body_util::Full;
use hyper::upgrade::Upgraded;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, watch};

use harness_core_api::{EventStream, HubEvent, RecvError};

use crate::Inner;

/// RFC 6455 handshake GUID.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Largest client frame accepted (pings and closes are tiny).
const MAX_FRAME: usize = 64 * 1024;

const OP_TEXT: u8 = 0x1;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// Close code 1008 "policy violation" (used when a device is revoked).
const CLOSE_POLICY: [u8; 2] = 1_008_u16.to_be_bytes();
/// Close code 1000 "normal closure".
const CLOSE_NORMAL: [u8; 2] = 1_000_u16.to_be_bytes();

/// True when the request asks for a WebSocket upgrade.
pub(crate) fn is_upgrade(headers: &http::HeaderMap) -> bool {
    let upgrade = headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let connection = headers
        .get(header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        });
    upgrade && connection
}

/// 101 response carrying `Sec-WebSocket-Accept`; `None` when the client key is unusable.
pub(crate) fn handshake(key: &str) -> Option<Response<Full<Bytes>>> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let accept = accept_key(key);
    let mut res = Response::new(Full::default());
    *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    res.headers_mut()
        .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    res.headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    let value = HeaderValue::from_str(&accept).ok()?;
    res.headers_mut()
        .insert(header::SEC_WEBSOCKET_ACCEPT, value);
    Some(res)
}

/// `base64(sha1(key + GUID))` — the RFC 6455 accept value.
pub(crate) fn accept_key(key: &str) -> String {
    let digest = sha1(format!("{key}{GUID}").as_bytes());
    base64::engine::general_purpose::STANDARD.encode(digest)
}

/// Pushes hub events to one client until it closes, its device is revoked or the server stops.
pub(crate) async fn serve(
    upgraded: Upgraded,
    inner: Arc<Inner>,
    device: String,
    mut events: EventStream,
    mut closing: watch::Receiver<bool>,
) {
    let mut revoked = inner.auth.revoked_rx();
    let io = hyper_util::rt::TokioIo::new(upgraded);
    let (mut reader, mut writer) = tokio::io::split(io);
    if send_json(&mut writer, &json!({ "type": "hello", "seq": inner.seq() }))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            _ = closing.changed() => {
                break;
            }
            revoked_device = revoked.recv() => {
                match revoked_device {
                    Ok(id) if id == device => {
                        let _ = write_frame(&mut writer, OP_CLOSE, &CLOSE_POLICY).await;
                        break;
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            frame = read_frame(&mut reader) => {
                match frame {
                    Ok(Some(ClientFrame::Ping(payload))) => {
                        if write_frame(&mut writer, OP_PONG, &payload).await.is_err() {
                            break;
                        }
                    }
                    Ok(Some(ClientFrame::Close | ClientFrame::Other) | None) | Err(_) => {
                        let _ = write_frame(&mut writer, OP_CLOSE, &CLOSE_NORMAL).await;
                        break;
                    }
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if send_event(&mut writer, &inner, &event).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        // The client re-opens its session snapshot after this frame.
                        if send_json(&mut writer, &json!({ "type": "resync" })).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => {
                        let _ = write_frame(&mut writer, OP_CLOSE, &CLOSE_NORMAL).await;
                        break;
                    }
                }
            }
        }
    }
}

async fn send_event<W>(writer: &mut W, inner: &Inner, event: &HubEvent) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    inner.set_seq(event.seq);
    send_json(writer, &json!({ "type": "event", "event": event })).await
}

async fn send_json<W>(writer: &mut W, value: &serde_json::Value) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    write_frame(writer, OP_TEXT, &payload).await
}

/// One unmasked server→client frame.
async fn write_frame<W>(writer: &mut W, opcode: u8, payload: &[u8]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut header = Vec::with_capacity(10);
    header.push(0x80 | opcode);
    if payload.len() < 126 {
        header.push(u8::try_from(payload.len()).unwrap_or(125));
    } else if let Ok(short) = u16::try_from(payload.len()) {
        header.push(126);
        header.extend_from_slice(&short.to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(
            &u64::try_from(payload.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
    }
    writer.write_all(&header).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

/// The client frames this server cares about.
#[derive(Debug)]
enum ClientFrame {
    Ping(Vec<u8>),
    Close,
    /// Text/binary/continuation frames are ignored: the server is the only sender of data.
    Other,
}

/// Reads one masked client frame; `Ok(None)` at a clean EOF.
async fn read_frame<R>(reader: &mut R) -> std::io::Result<Option<ClientFrame>>
where
    R: AsyncRead + Unpin,
{
    let mut head = [0_u8; 2];
    if let Err(error) = reader.read_exact(&mut head).await {
        return if error.kind() == std::io::ErrorKind::UnexpectedEof {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let opcode = head[0] & 0x0f;
    let masked = head[1] & 0x80 != 0;
    let length = match head[1] & 0x7f {
        126 => {
            let mut raw = [0_u8; 2];
            reader.read_exact(&mut raw).await?;
            usize::from(u16::from_be_bytes(raw))
        }
        127 => {
            let mut raw = [0_u8; 8];
            reader.read_exact(&mut raw).await?;
            usize::try_from(u64::from_be_bytes(raw)).unwrap_or(usize::MAX)
        }
        short => usize::from(short),
    };
    if length > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut mask = [0_u8; 4];
    if masked {
        reader.read_exact(&mut mask).await?;
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    if masked {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }
    Ok(Some(match opcode {
        OP_PING => ClientFrame::Ping(payload),
        OP_CLOSE => ClientFrame::Close,
        _ => ClientFrame::Other,
    }))
}

/// SHA-1 (FIPS 180-4). Only the WS handshake needs it; `sha2` has no SHA-1 variant.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = u64::try_from(data.len())
        .unwrap_or(u64::MAX)
        .wrapping_mul(8);
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut words = [0_u32; 80];
        for (index, chunk) in block.chunks_exact(4).enumerate() {
            let byte = |offset: usize| u32::from(chunk.get(offset).copied().unwrap_or(0));
            words[index] = (byte(0) << 24) | (byte(1) << 16) | (byte(2) << 8) | byte(3);
        }
        for index in 16..80 {
            let mixed = words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16];
            words[index] = mixed.rotate_left(1);
        }
        let mut w = state;
        for (index, word) in words.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((w[1] & w[2]) | (!w[1] & w[3]), 0x5A82_7999_u32),
                20..=39 => (w[1] ^ w[2] ^ w[3], 0x6ED9_EBA1),
                40..=59 => ((w[1] & w[2]) | (w[1] & w[3]) | (w[2] & w[3]), 0x8F1B_BCDC),
                _ => (w[1] ^ w[2] ^ w[3], 0xCA62_C1D6),
            };
            let temp = w[0]
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(w[4])
                .wrapping_add(k)
                .wrapping_add(*word);
            w[4] = w[3];
            w[3] = w[2];
            w[2] = w[1].rotate_left(30);
            w[1] = w[0];
            w[0] = temp;
        }
        state[0] = state[0].wrapping_add(w[0]);
        state[1] = state[1].wrapping_add(w[1]);
        state[2] = state[2].wrapping_add(w[2]);
        state[3] = state[3].wrapping_add(w[3]);
        state[4] = state[4].wrapping_add(w[4]);
    }
    let mut digest = [0_u8; 20];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests;
