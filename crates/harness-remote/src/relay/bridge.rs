//! One phone behind the relay: the E2E handshake, then the inner protocol (docs/plans/remote-relay.md §4)
//! bridged onto this crate's own loopback server, so routes, device tokens and pairing stay as they are.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use bytes::Bytes;
use futures_util::{SinkExt as _, StreamExt as _};
use http::{header, HeaderValue, Method, Request};
use http_body_util::{BodyExt as _, Full, Limited};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Semaphore};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::Message;

use super::crypto::{Assembler, Keys, HELLO, MAX_MESSAGE, SALT_LEN};
use super::Out;

const MAX_STREAMS: usize = 8;
const MAX_REQUESTS: usize = 16;
/// Request headers a phone may set; everything else is dropped.
const PASS_HEADERS: [header::HeaderName; 3] = [
    header::CONTENT_TYPE,
    header::AUTHORIZATION,
    header::HeaderName::from_static("x-file-name"),
];

/// Runs one phone until it disconnects or breaks the protocol (then it is kicked).
pub(crate) async fn phone(
    cid: u32,
    secret: [u8; 32],
    local: SocketAddr,
    mut inbound: mpsc::Receiver<Vec<u8>>,
    out: mpsc::Sender<Out>,
) {
    let Some(hello) = inbound.recv().await else {
        return;
    };
    if hello.len() != 1 + SALT_LEN || hello[0] != HELLO {
        let _ = out.send(Out::Kick(cid)).await;
        return;
    }
    let mut salt_h = [0_u8; SALT_LEN];
    if getrandom::fill(&mut salt_h).is_err() {
        let _ = out.send(Out::Kick(cid)).await;
        return;
    }
    let peer = peer_ip(salt_h);
    let mut keys = Keys::host(&secret, &hello[1..], &salt_h);
    if out
        .send(Out::Frame(cid, [&[HELLO], salt_h.as_slice()].concat()))
        .await
        .is_err()
    {
        return;
    }
    let (reply_tx, mut replies) = mpsc::unbounded_channel::<Value>();
    let mut asm = Assembler::default();
    let mut streams: HashMap<u64, mpsc::UnboundedSender<String>> = HashMap::new();
    let requests = Arc::new(Semaphore::new(MAX_REQUESTS));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            frame = inbound.recv() => {
                let Some(frame) = frame else { break };
                let message = keys.open(&frame).map(|plain| asm.push(&plain));
                match message {
                    Some(Ok(Some(message))) => {
                        let Ok(value) = serde_json::from_slice::<Value>(&message) else { continue };
                        let ctx = Ctx { local, peer: &peer, reply: &reply_tx };
                        handle(value, &ctx, &mut streams, &requests, &mut tasks);
                    }
                    Some(Ok(None)) => {}
                    _ => {
                        let _ = out.send(Out::Kick(cid)).await;
                        break;
                    }
                }
            }
            Some(reply) = replies.recv() => {
                let bytes = serde_json::to_vec(&reply).unwrap_or_default();
                for frame in keys.seal_message(&bytes) {
                    if out.send(Out::Frame(cid, frame)).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
    tasks.abort_all();
}

/// Rate-limit key the loopback server sees for one relay phone connection: a random address in the
/// carrier-grade NAT range, so one misbehaving phone cannot lock every other relay phone out.
// 安全修复：之前所有手机共用 100.64.0.1，一个手机失败 10 次就会把所有中继手机都锁住。
fn peer_ip(random: [u8; SALT_LEN]) -> String {
    format!("100.{}.{}.{}", 64 + (random[0] & 63), random[1], random[2])
}

struct Ctx<'a> {
    local: SocketAddr,
    peer: &'a str,
    reply: &'a mpsc::UnboundedSender<Value>,
}

fn handle(
    message: Value,
    ctx: &Ctx<'_>,
    streams: &mut HashMap<u64, mpsc::UnboundedSender<String>>,
    requests: &Arc<Semaphore>,
    tasks: &mut tokio::task::JoinSet<()>,
) {
    let (local, peer, reply) = (ctx.local, ctx.peer.to_owned(), ctx.reply);
    let id = message.get("id").and_then(Value::as_u64).unwrap_or(0);
    match message.get("t").and_then(Value::as_str) {
        Some("req") => {
            let reply = reply.clone();
            let Ok(permit) = Arc::clone(requests).try_acquire_owned() else {
                let _ = reply.send(
                    json!({ "t": "res", "id": id, "status": 429, "headers": {}, "body": "" }),
                );
                return;
            };
            tasks.spawn(async move {
                let res = request(local, &peer, &message).await.unwrap_or_else(
                    |status| json!({ "status": status, "headers": {}, "body": "" }),
                );
                let mut res = res;
                res["t"] = json!("res");
                res["id"] = json!(id);
                let _ = reply.send(res);
                drop(permit);
            });
        }
        Some("ws-open") => {
            if streams.len() >= MAX_STREAMS || streams.contains_key(&id) {
                let _ = reply.send(json!({ "t": "ws-close", "id": id }));
                return;
            }
            let (tx, rx) = mpsc::unbounded_channel();
            streams.insert(id, tx);
            let reply = reply.clone();
            tasks.spawn(async move {
                stream(local, &peer, &message, rx, &reply, id).await;
                let _ = reply.send(json!({ "t": "ws-close", "id": id }));
            });
        }
        Some("ws") => {
            if let (Some(tx), Some(data)) = (
                streams.get(&id),
                message.get("data").and_then(Value::as_str),
            ) {
                let _ = tx.send(data.to_owned());
            }
        }
        Some("ws-close") => {
            streams.remove(&id);
        }
        _ => {}
    }
    streams.retain(|_, tx| !tx.is_closed());
}

/// Only API routes go to the desktop; the client's static files come from the relay.
fn api_path(message: &Value) -> Option<&str> {
    let path = message.get("path").and_then(Value::as_str)?;
    let ok = (path.starts_with("/api/v1/") || path == "/healthz")
        && !path.contains("..")
        && path.bytes().all(|b| b.is_ascii_graphic());
    ok.then_some(path)
}

fn passed_headers(message: &Value) -> Vec<(header::HeaderName, HeaderValue)> {
    let Some(headers) = message.get("headers").and_then(Value::as_object) else {
        return Vec::new();
    };
    PASS_HEADERS
        .iter()
        .filter_map(|name| {
            let value = headers.get(name.as_str()).and_then(Value::as_str)?;
            Some((name.clone(), HeaderValue::from_str(value).ok()?))
        })
        .collect()
}

async fn request(local: SocketAddr, peer: &str, message: &Value) -> Result<Value, u16> {
    let path = api_path(message).ok_or(404_u16)?;
    let method = match message.get("method").and_then(Value::as_str) {
        Some("GET") => Method::GET,
        Some("POST") => Method::POST,
        _ => return Err(405),
    };
    let body = match message.get("body").and_then(Value::as_str) {
        Some(text) if !text.is_empty() => STANDARD.decode(text).map_err(|_| 400_u16)?,
        _ => Vec::new(),
    };
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, local.to_string())
        .header("cf-connecting-ip", peer);
    for (name, value) in passed_headers(message) {
        req = req.header(name, value);
    }
    let req = req
        .body(Full::new(Bytes::from(body)))
        .map_err(|_| 400_u16)?;
    let tcp = TcpStream::connect(local).await.map_err(|_| 502_u16)?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
        .await
        .map_err(|_| 502_u16)?;
    tokio::spawn(conn);
    let res = sender.send_request(req).await.map_err(|_| 502_u16)?;
    let status = res.status().as_u16();
    let mut headers = serde_json::Map::new();
    for name in [header::CONTENT_TYPE, header::SET_COOKIE] {
        if let Some(value) = res.headers().get(&name).and_then(|v| v.to_str().ok()) {
            headers.insert(name.as_str().to_owned(), json!(value));
        }
    }
    let body = Limited::new(res.into_body(), MAX_MESSAGE / 2)
        .collect()
        .await
        .map_err(|_| 502_u16)?
        .to_bytes();
    Ok(json!({ "status": status, "headers": headers, "body": STANDARD.encode(body) }))
}

/// A WebSocket on the loopback server (the event stream), relayed message by message.
async fn stream(
    local: SocketAddr,
    peer: &str,
    message: &Value,
    mut from_phone: mpsc::UnboundedReceiver<String>,
    reply: &mpsc::UnboundedSender<Value>,
    id: u64,
) {
    let Some(path) = api_path(message) else {
        return;
    };
    let Ok(mut req) = format!("ws://{local}{path}").into_client_request() else {
        return;
    };
    for (name, value) in passed_headers(message) {
        req.headers_mut().insert(name, value);
    }
    if let Ok(peer) = HeaderValue::from_str(peer) {
        req.headers_mut().insert("cf-connecting-ip", peer);
    }
    let Ok(tcp) = TcpStream::connect(local).await else {
        return;
    };
    let Ok((socket, _)) = tokio_tungstenite::client_async(req, tcp).await else {
        return;
    };
    let (mut sink, mut source) = socket.split();
    loop {
        tokio::select! {
            incoming = source.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if reply.send(json!({ "t": "ws", "id": id, "data": text.as_str() })).is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            outgoing = from_phone.recv() => {
                let Some(text) = outgoing else {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                };
                if sink.send(Message::text(text)).await.is_err() {
                    break;
                }
            },
        }
    }
}
