use super::*;

#[tokio::test]
async fn status_requires_a_device_token() {
    let server = Server::start().await;
    let no_token = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &[],
        "",
    )
    .await;
    assert_eq!(no_token.status, 401);
    assert!(no_token.body.is_empty(), "401 must have an empty body");

    let empty = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &cookie_header("harness_remote="),
        "",
    )
    .await;
    assert_eq!(empty.status, 401);

    let wrong = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &cookie_header("harness_remote=deadbeef"),
        "",
    )
    .await;
    assert_eq!(wrong.status, 401);

    let (device_id, cookie) = server.pair("Pixel").await;
    let ok = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &cookie_header(&cookie),
        "",
    )
    .await;
    assert_eq!(ok.status, 200);
    let value = ok.json();
    assert_eq!(value["device_id"], json!(device_id));
    assert_eq!(value["api_version"], json!(1));
    assert_eq!(value["hub_ready"], json!(true));
    // The bearer header works for non-cookie clients.
    let bearer = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &[(
            "Authorization",
            format!("Bearer {}", cookie.split_once('=').unwrap().1),
        )],
        "",
    )
    .await;
    assert_eq!(bearer.status, 200);
}

#[tokio::test]
async fn pair_by_token_and_by_code_is_single_use() {
    let server = Server::start().await;

    let offer = server.handle.new_pair_offer("Pixel");
    assert_eq!(offer.token.len(), 64);
    assert_eq!(offer.code.len(), 8);
    let first = call(
        server.addr,
        "POST",
        "/api/v1/pair",
        &server.host(),
        &[],
        &json!({ "token": offer.token }).to_string(),
    )
    .await;
    assert_eq!(first.status, 200);
    assert!(first.cookie.is_some());
    let reused = call(
        server.addr,
        "POST",
        "/api/v1/pair",
        &server.host(),
        &[],
        &json!({ "token": offer.token }).to_string(),
    )
    .await;
    assert_eq!(reused.status, 401, "pair tokens are single use");

    // Short codes are typed in lower case and must still redeem.
    let code_offer = server.handle.new_pair_offer("Tablet");
    let by_code = call(
        server.addr,
        "POST",
        "/api/v1/pair",
        &server.host(),
        &[],
        &json!({ "code": code_offer.code.to_lowercase(), "name": "Tablet" }).to_string(),
    )
    .await;
    assert_eq!(by_code.status, 200);
    assert_eq!(server.handle.devices().len(), 2);
    let again = call(
        server.addr,
        "POST",
        "/api/v1/pair",
        &server.host(),
        &[],
        &json!({ "code": code_offer.code }).to_string(),
    )
    .await;
    assert_eq!(again.status, 401);
}

#[tokio::test]
async fn expired_pair_offer_is_rejected() {
    let server = Server::start().await;
    let offer = server.handle.new_pair_offer("Old phone");
    server.handle.inner.auth.expire(&offer.token);
    let reply = call(
        server.addr,
        "POST",
        "/api/v1/pair",
        &server.host(),
        &[],
        &json!({ "token": offer.token }).to_string(),
    )
    .await;
    assert_eq!(reply.status, 401);
    assert_eq!(server.handle.devices().len(), 0);
}

#[tokio::test]
async fn eleven_bad_attempts_get_429() {
    let server = Server::start().await;
    for attempt in 1..=10 {
        let reply = call(
            server.addr,
            "GET",
            "/api/v1/status",
            &server.host(),
            &cookie_header("harness_remote=wrong"),
            "",
        )
        .await;
        assert_eq!(reply.status, 401, "attempt {attempt}");
    }
    let limited = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &cookie_header("harness_remote=wrong"),
        "",
    )
    .await;
    assert_eq!(limited.status, 429);
    // Pairing from the same IP is limited too (counted before the credential check).
    let pair = call(
        server.addr,
        "POST",
        "/api/v1/pair",
        &server.host(),
        &[],
        &json!({ "token": "x" }).to_string(),
    )
    .await;
    assert_eq!(pair.status, 429);
}

#[tokio::test]
async fn evil_host_and_origin_are_rejected() {
    let server = Server::start().await;
    let (_device_id, cookie) = server.pair("Pixel").await;
    let evil = call(
        server.addr,
        "GET",
        "/api/v1/status",
        "evil.example.com",
        &cookie_header(&cookie),
        "",
    )
    .await;
    assert_eq!(evil.status, 403);
    let evil_static = call(server.addr, "GET", "/", "evil.example.com", &[], "").await;
    assert_eq!(evil_static.status, 403);

    let bad_origin = call(
        server.addr,
        "POST",
        "/api/v1/rpc",
        &server.host(),
        &[
            ("Origin", "https://evil.example.com".to_owned()),
            cookie_header(&cookie)[0].clone(),
        ],
        &json!({ "method": "list_sessions", "params": {} }).to_string(),
    )
    .await;
    assert_eq!(bad_origin.status, 403);

    // The configured public host is accepted (tunnel case).
    server
        .handle
        .set_public_host(Some("harness.example.com".to_owned()));
    let tunnel = call(
        server.addr,
        "GET",
        "/api/v1/status",
        "harness.example.com",
        &cookie_header(&cookie),
        "",
    )
    .await;
    assert_eq!(tunnel.status, 200);
    let tunnel_origin = call(
        server.addr,
        "POST",
        "/api/v1/rpc",
        "harness.example.com",
        &[
            ("Origin", "https://harness.example.com".to_owned()),
            cookie_header(&cookie)[0].clone(),
        ],
        &json!({ "method": "list_sessions", "params": {} }).to_string(),
    )
    .await;
    assert_eq!(tunnel_origin.status, 200);
}
