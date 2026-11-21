use super::{
    header, Arc, Body, BodyExt, Bytes, HeaderMap, HeaderValue, Incoming, Inner, Limited, Request,
    Response, SocketAddr, StatusCode, Value, COOKIE, JSON,
};

/// `Origin` (when present on a `POST`) must belong to this server.
pub(super) fn origin_ok(inner: &Inner, headers: &HeaderMap) -> bool {
    match header_str(headers, header::ORIGIN) {
        None => true,
        Some(origin) => inner.origin_allowed(origin),
    }
}

/// Device auth of a request; the `Err` status is 429 when the IP is rate limited, else 401.
pub(super) fn authenticate(
    inner: &Arc<Inner>,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> Result<String, StatusCode> {
    if let Some(device) = token_of(headers).and_then(|token| inner.auth.device_for_token(&token)) {
        return Ok(device);
    }
    let ip = client_ip(peer, headers);
    if !inner.auth.allow(ip) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    inner.auth.fail(ip);
    Err(StatusCode::UNAUTHORIZED)
}

/// Rate-limit key: behind a local `cloudflared` every peer is loopback, so the tunnel's
/// `CF-Connecting-IP` names the real client. Only trusted when the peer itself is loopback.
pub(crate) fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> std::net::IpAddr {
    let ip = peer.ip();
    if ip.is_loopback() {
        if let Some(forwarded) = header_str(headers, "cf-connecting-ip")
            .and_then(|value| value.trim().parse::<std::net::IpAddr>().ok())
        {
            return forwarded;
        }
    }
    ip
}

/// Cookie first (browsers cannot set headers on `new WebSocket`), then `Authorization: Bearer`.
pub(super) fn token_of(headers: &HeaderMap) -> Option<String> {
    if let Some(cookies) = header_str(headers, header::COOKIE) {
        for part in cookies.split(';') {
            let part = part.trim();
            if let Some(value) = part.strip_prefix(&format!("{COOKIE}=")) {
                let value = value.trim_matches('"');
                if !value.is_empty() {
                    return Some(value.to_owned());
                }
            }
        }
    }
    let auth = header_str(headers, header::AUTHORIZATION)?;
    let (scheme, value) = auth.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") || value.trim().is_empty() {
        return None;
    }
    Some(value.trim().to_owned())
}

pub(super) fn header_str(
    headers: &HeaderMap,
    name: impl http::header::AsHeaderName,
) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Reads the body, refusing anything over `limit` (`None` → 413).
pub(super) async fn read_body(req: Request<Incoming>, limit: usize) -> Option<Bytes> {
    let length = header_str(req.headers(), header::CONTENT_LENGTH)
        .and_then(|value| value.parse::<usize>().ok());
    if length.is_some_and(|length| length > limit) {
        return None;
    }
    let limited = Limited::new(req.into_body(), limit);
    limited
        .collect()
        .await
        .ok()
        .map(http_body_util::Collected::to_bytes)
}

pub(super) fn empty(code: StatusCode) -> Response<Body> {
    let mut res = Response::new(Body::default());
    *res.status_mut() = code;
    res
}

pub(super) fn json_response(code: StatusCode, value: &Value) -> Response<Body> {
    let mut res = Response::new(Body::from(Bytes::from(
        serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec()),
    )));
    *res.status_mut() = code;
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON));
    res
}
