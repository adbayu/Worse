//! In-crate integration tests: a stub `HarnessCore`, the real loopback server, plain HTTP/1.1 and
//! WebSocket clients written against the socket (no extra test dependencies).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use harness_core_api::{
    AgentId, ApiError, ApiResult, BoxFuture, CreateSession, EventKind, EventStream, HarnessCore,
    HubEvent, ListSessions, MemoryScope, MemoryWriteMode, PermissionRequestId, SendMessage,
    SessionId, SessionSummary, TurnId,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;

use crate::{RemoteConfig, RemoteHandle};

mod stub;

use stub::{StubCore, TIMEOUT};

mod lockout;
mod pairing;
mod rpc_upload;
mod socket;

// ---------------------------------------------------------------- harness

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Server {
    handle: RemoteHandle,
    addr: SocketAddr,
    core: Arc<StubCore>,
    dir: PathBuf,
}

impl Server {
    async fn start() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "harness-remote-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let core = StubCore::new();
        let handle = RemoteHandle::start(
            RemoteConfig {
                port: 0,
                state_dir: dir.clone(),
            },
            core.clone(),
        )
        .await
        .unwrap();
        Self {
            addr: handle.local_addr(),
            handle,
            core,
            dir,
        }
    }

    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }

    /// Pairs one device through the real `/api/v1/pair` route and returns `(device_id, cookie)`.
    async fn pair(&self, name: &str) -> (String, String) {
        let offer = self.handle.new_pair_offer(name);
        let reply = call(
            self.addr,
            "POST",
            "/api/v1/pair",
            &self.host(),
            &[],
            &json!({ "token": offer.token, "name": name }).to_string(),
        )
        .await;
        assert_eq!(reply.status, 200, "pair failed: {:?}", reply.text());
        let value: Value = serde_json::from_slice(&reply.body).unwrap();
        let device_id = value["device_id"].as_str().unwrap().to_owned();
        let cookie = reply.cookie.unwrap();
        (device_id, cookie)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------- HTTP client

struct Reply {
    status: u16,
    head: String,
    body: Vec<u8>,
    cookie: Option<String>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

async fn call(
    addr: SocketAddr,
    method: &str,
    path: &str,
    host: &str,
    extra: &[(&str, String)],
    body: &str,
) -> Reply {
    send(addr, method, path, host, extra, body.len(), body).await
}

/// Like [`call`], but with a declared `Content-Length` the body does not have: the server must
/// refuse the oversized body from the header alone (413) instead of the test writing 25 MB.
async fn call_declared(
    addr: SocketAddr,
    path: &str,
    host: &str,
    extra: &[(&str, String)],
    declared: usize,
) -> Reply {
    send(addr, "POST", path, host, extra, declared, "").await
}

async fn send(
    addr: SocketAddr,
    method: &str,
    path: &str,
    host: &str,
    extra: &[(&str, String)],
    declared: usize,
    body: &str,
) -> Reply {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {declared}\r\n"
    );
    for (name, value) in extra {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body);
    let _ = stream.write_all(request.as_bytes()).await;
    let raw = read_to_end(&mut stream).await;
    parse(&raw)
}

pub(crate) async fn read_to_end(stream: &mut TcpStream) -> Vec<u8> {
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut raw)).await;
    raw
}

fn parse(raw: &[u8]) -> Reply {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = if split + 4 <= raw.len() {
        raw[split + 4..].to_vec()
    } else {
        Vec::new()
    };
    let mut lines = head.lines();
    let head = head.clone();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let cookie = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        .and_then(|(_, value)| value.trim().split(';').next())
        .map(str::to_owned);
    Reply {
        status,
        head,
        body,
        cookie,
    }
}

fn cookie_header(cookie: &str) -> Vec<(&'static str, String)> {
    vec![("Cookie", cookie.to_owned())]
}

// ---------------------------------------------------------------- WS client

/// Opens `/api/v1/events`, returns the raw stream once the 101 response is read.
async fn ws_connect(addr: SocketAddr, host: &str, cookie: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET /api/v1/events HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\n\
         Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nCookie: {cookie}\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let read = tokio::time::timeout(TIMEOUT, stream.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read, 1);
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    assert!(head.starts_with("HTTP/1.1 101"), "handshake failed: {head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("sec-websocket-accept: s3pplmbitxaq9kygzzhzrbk+xoo="),
        "missing accept header: {head}"
    );
    stream
}

/// Reads one server→client frame: `(opcode, payload)`.
async fn ws_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut head = [0_u8; 2];
    tokio::time::timeout(TIMEOUT, stream.read_exact(&mut head))
        .await
        .unwrap()
        .unwrap();
    let opcode = head[0] & 0x0f;
    let length = match head[1] & 0x7f {
        126 => {
            let mut raw = [0_u8; 2];
            stream.read_exact(&mut raw).await.unwrap();
            usize::from(u16::from_be_bytes(raw))
        }
        127 => {
            let mut raw = [0_u8; 8];
            stream.read_exact(&mut raw).await.unwrap();
            usize::try_from(u64::from_be_bytes(raw)).unwrap()
        }
        short => usize::from(short),
    };
    let mut payload = vec![0_u8; length];
    tokio::time::timeout(TIMEOUT, stream.read_exact(&mut payload))
        .await
        .unwrap()
        .unwrap();
    (opcode, payload)
}

async fn ws_json(stream: &mut TcpStream) -> Value {
    let (opcode, payload) = ws_frame(stream).await;
    assert_eq!(opcode, 0x1, "expected a text frame");
    serde_json::from_slice(&payload).unwrap()
}

// ---------------------------------------------------------------- tests
