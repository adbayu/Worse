use super::*;

#[tokio::test]
async fn valid_device_token_bypasses_the_lockout() {
    let server = Server::start().await;
    let (_device_id, cookie) = server.pair("Pixel").await;
    for _ in 0..11 {
        call(
            server.addr,
            "GET",
            "/api/v1/status",
            &server.host(),
            &cookie_header("harness_remote=wrong"),
            "",
        )
        .await;
    }
    let bad = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &cookie_header("harness_remote=wrong"),
        "",
    )
    .await;
    assert_eq!(bad.status, 429);
    let good = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &cookie_header(&cookie),
        "",
    )
    .await;
    assert_eq!(good.status, 200);
}

#[tokio::test]
async fn cf_connecting_ip_keys_the_rate_limit_behind_the_tunnel() {
    let server = Server::start().await;
    let from = |ip: &str| {
        let mut headers = cookie_header("harness_remote=wrong");
        headers.push(("CF-Connecting-IP", ip.to_owned()));
        headers
    };
    for _ in 0..11 {
        call(
            server.addr,
            "GET",
            "/api/v1/status",
            &server.host(),
            &from("203.0.113.7"),
            "",
        )
        .await;
    }
    let attacker = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &from("203.0.113.7"),
        "",
    )
    .await;
    assert_eq!(attacker.status, 429);
    let attacker_pair = call(
        server.addr,
        "POST",
        "/api/v1/pair",
        &server.host(),
        &[("CF-Connecting-IP", "203.0.113.7".to_owned())],
        &json!({ "token": "x" }).to_string(),
    )
    .await;
    assert_eq!(attacker_pair.status, 429);

    let other = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &from("198.51.100.2"),
        "",
    )
    .await;
    assert_eq!(other.status, 401);
    let (_device_id, _cookie) = server.pair("Pixel").await;
}

#[test]
fn cf_connecting_ip_is_trusted_only_from_loopback() {
    use crate::http::client_ip;
    let mut headers = http::HeaderMap::new();
    headers.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());
    let loopback: SocketAddr = "127.0.0.1:5000".parse().unwrap();
    let remote: SocketAddr = "192.0.2.9:5000".parse().unwrap();
    assert_eq!(client_ip(loopback, &headers).to_string(), "203.0.113.7");
    assert_eq!(client_ip(remote, &headers).to_string(), "192.0.2.9");
    headers.insert("cf-connecting-ip", "not-an-ip".parse().unwrap());
    assert_eq!(client_ip(loopback, &headers).to_string(), "127.0.0.1");
    assert_eq!(
        client_ip(loopback, &http::HeaderMap::new()).to_string(),
        "127.0.0.1"
    );
}
