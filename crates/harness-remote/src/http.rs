//! Routing, static assets, `Host`/`Origin` checks, the body caps and the phone upload route.

use std::convert::Infallible;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderValue};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};

use crate::{rpc, ws, Inner};

mod assets;
mod upload;
mod util;

use assets::asset;

use upload::upload;
pub(crate) use util::client_ip;
use util::{authenticate, empty, header_str, json_response, origin_ok, read_body};

type Body = Full<Bytes>;

/// Largest accepted JSON body (1 MB) for every route but `/api/v1/rpc`.
const MAX_BODY: usize = 1024 * 1024;
/// `/api/v1/rpc` cap: base64 images travel inside `send_message` (`chat::limits`).
const MAX_RPC_BODY: usize = 16 * 1024 * 1024;
/// `/api/v1/upload` cap: raw attachment bytes.
const MAX_UPLOAD_BODY: usize = 25 * 1024 * 1024;
const JSON: &str = "application/json";
const COOKIE: &str = "harness_remote";

/// Accept loop; runs until the handle's `closing` watch flips.
pub(crate) async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    let mut closing = inner.closing_rx();
    loop {
        tokio::select! {
            _ = closing.changed() => break,
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                let _ = stream.set_nodelay(true);
                let conn = Arc::clone(&inner);
                tokio::spawn(async move { serve(conn, stream, peer).await });
            }
        }
    }
}

async fn serve(inner: Arc<Inner>, stream: TcpStream, peer: SocketAddr) {
    let mut closing = inner.closing_rx();
    let service = service_fn(move |req: Request<Incoming>| {
        let inner = Arc::clone(&inner);
        async move { Ok::<_, Infallible>(route(&inner, peer, req).await) }
    });
    let conn = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades();
    tokio::pin!(conn);
    tokio::select! {
        _ = conn.as_mut() => {}
        _ = closing.changed() => {
            conn.as_mut().graceful_shutdown();
            let _ = conn.await;
        }
    }
}

async fn route(inner: &Arc<Inner>, peer: SocketAddr, req: Request<Incoming>) -> Response<Body> {
    // Host first: DNS rebinding is refused before anything else happens.
    let Some(host) = header_str(req.headers(), header::HOST) else {
        return empty(StatusCode::FORBIDDEN);
    };
    if !inner.host_allowed(host) {
        return empty(StatusCode::FORBIDDEN);
    }
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    match (method, path.as_str()) {
        (Method::GET, "/" | "/p") => static_file("index.html"),
        (Method::GET, "/healthz") => json_response(StatusCode::OK, &json!({ "ok": true })),
        (Method::GET, "/api/v1/status") => status(inner, peer, &req),
        (Method::GET, "/api/v1/events") => events(inner, peer, req),
        (Method::POST, "/api/v1/pair") => pair(inner, peer, req).await,
        (Method::POST, "/api/v1/rpc") => rpc_call(inner, peer, req).await,
        (Method::POST, "/api/v1/upload") => upload(inner, peer, req).await,
        (Method::POST, "/api/v1/devices/revoke") => revoke(inner, peer, req).await,
        (Method::GET, other) => static_route(other),
        _ => empty(StatusCode::NOT_FOUND),
    }
}

fn static_route(path: &str) -> Response<Body> {
    let name = path
        .strip_prefix("/assets/")
        .unwrap_or_else(|| path.strip_prefix('/').unwrap_or(path));
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return empty(StatusCode::NOT_FOUND);
    }
    static_file(name)
}

fn static_file(name: &str) -> Response<Body> {
    let Some(asset) = asset(name) else {
        return empty(StatusCode::NOT_FOUND);
    };
    let mut res = Response::new(Body::from(Bytes::from_static(asset.body)));
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if asset.name == "sw.js" {
        res.headers_mut().insert(
            header::HeaderName::from_static("service-worker-allowed"),
            HeaderValue::from_static("/"),
        );
    }
    res
}

async fn pair(inner: &Arc<Inner>, peer: SocketAddr, req: Request<Incoming>) -> Response<Body> {
    let ip = client_ip(peer, req.headers());
    if !inner.auth.allow(ip) {
        return empty(StatusCode::TOO_MANY_REQUESTS);
    }
    if !origin_ok(inner, req.headers()) {
        return empty(StatusCode::FORBIDDEN);
    }
    let Some(body) = read_body(req, MAX_BODY).await else {
        return empty(StatusCode::PAYLOAD_TOO_LARGE);
    };
    let Ok(params) = serde_json::from_slice::<Value>(&body) else {
        inner.auth.fail(ip);
        return json_response(
            StatusCode::UNAUTHORIZED,
            &json!({ "ok": false, "error": "invalid body" }),
        );
    };
    let token = params.get("token").and_then(Value::as_str);
    let code = params.get("code").and_then(Value::as_str);
    let name = params.get("name").and_then(Value::as_str);
    if token.is_none() && code.is_none() {
        inner.auth.fail(ip);
        return json_response(
            StatusCode::UNAUTHORIZED,
            &json!({ "ok": false, "error": "token or code required" }),
        );
    }
    let Some((device_id, device_token)) = inner.auth.pair(token, code, name) else {
        inner.auth.fail(ip);
        return json_response(
            StatusCode::UNAUTHORIZED,
            &json!({ "ok": false, "error": "unknown or expired pair offer" }),
        );
    };
    let mut res = json_response(
        StatusCode::OK,
        &json!({ "ok": true, "device_id": device_id }),
    );
    let cookie = format!("{COOKIE}={device_token}; HttpOnly; Secure; SameSite=Strict; Path=/");
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        res.headers_mut().append(header::SET_COOKIE, value);
    }
    res
}

fn status(inner: &Arc<Inner>, peer: SocketAddr, req: &Request<Incoming>) -> Response<Body> {
    let device = match authenticate(inner, peer, req.headers()) {
        Ok(device) => device,
        Err(status) => return empty(status),
    };
    json_response(
        StatusCode::OK,
        &json!({
            "version": env!("CARGO_PKG_VERSION"),
            "api_version": 1,
            "device_id": device,
            "hub_ready": true,
        }),
    )
}

async fn rpc_call(inner: &Arc<Inner>, peer: SocketAddr, req: Request<Incoming>) -> Response<Body> {
    if let Err(status) = authenticate(inner, peer, req.headers()) {
        return empty(status);
    }
    if !origin_ok(inner, req.headers()) {
        return empty(StatusCode::FORBIDDEN);
    }
    let Some(body) = read_body(req, MAX_RPC_BODY).await else {
        return empty(StatusCode::PAYLOAD_TOO_LARGE);
    };
    let Ok(params) = serde_json::from_slice::<Value>(&body) else {
        return json_response(StatusCode::OK, &rpc::bad_request("invalid json"));
    };
    let method = params
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let args = params.get("params").cloned().unwrap_or(Value::Null);
    if !rpc::is_allowed(&method) {
        return json_response(StatusCode::OK, &rpc::not_allowed(&method));
    }
    let view = inner.settings.read().ok().and_then(|slot| slot.clone());
    if let Some(result) = rpc::settings_call(view.as_deref(), &method, &args) {
        let response = match result {
            Ok(value) => json!({ "ok": true, "value": value }),
            Err(error) => json!({ "ok": false, "error": error }),
        };
        return json_response(StatusCode::OK, &response);
    }
    let response = match rpc::call(inner.core.as_ref(), &method, args).await {
        Ok(mut value) => {
            // Plan limits live in the desktop app (plan_usage), not the hub: merge them in.
            if method == "usage_stats" {
                value["limits"] = view.map_or_else(|| json!([]), |view| view.limits());
            }
            json!({ "ok": true, "value": value })
        }
        Err(error) => json!({ "ok": false, "error": error }),
    };
    json_response(StatusCode::OK, &response)
}

async fn revoke(inner: &Arc<Inner>, peer: SocketAddr, req: Request<Incoming>) -> Response<Body> {
    let device = match authenticate(inner, peer, req.headers()) {
        Ok(device) => device,
        Err(status) => return empty(status),
    };
    if !origin_ok(inner, req.headers()) {
        return empty(StatusCode::FORBIDDEN);
    }
    let Some(body) = read_body(req, MAX_BODY).await else {
        return empty(StatusCode::PAYLOAD_TOO_LARGE);
    };
    let Ok(params) = serde_json::from_slice::<Value>(&body) else {
        return json_response(
            StatusCode::OK,
            &json!({ "ok": false, "error": "invalid json" }),
        );
    };
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if id.is_empty() {
        return json_response(
            StatusCode::OK,
            &json!({ "ok": false, "error": "id required" }),
        );
    }
    // 安全修复：手机只能注销自己，其他设备只能在桌面端的设置里移除。
    if id != device {
        return json_response(
            StatusCode::FORBIDDEN,
            &json!({ "ok": false, "error": "a phone can only unpair itself" }),
        );
    }
    inner.auth.revoke(&id);
    json_response(StatusCode::OK, &json!({ "ok": true }))
}

fn events(inner: &Arc<Inner>, peer: SocketAddr, req: Request<Incoming>) -> Response<Body> {
    let device = match authenticate(inner, peer, req.headers()) {
        Ok(device) => device,
        Err(status) => return empty(status),
    };
    if !ws::is_upgrade(req.headers()) {
        return empty(StatusCode::UPGRADE_REQUIRED);
    }
    let Some(key) = header_str(req.headers(), "sec-websocket-key").map(str::to_owned) else {
        return empty(StatusCode::BAD_REQUEST);
    };
    let Some(res) = ws::handshake(&key) else {
        return empty(StatusCode::BAD_REQUEST);
    };
    let stream = inner.core.subscribe();
    let state = Arc::clone(inner);
    let closing = inner.closing_rx();
    tokio::spawn(async move {
        if let Ok(upgraded) = hyper::upgrade::on(req).await {
            ws::serve(upgraded, state, device, stream, closing).await;
        }
    });
    res
}
