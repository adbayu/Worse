use super::*;

#[tokio::test]
async fn websocket_sends_hello_then_event_frames() {
    let server = Server::start().await;
    let (_device_id, cookie) = server.pair("Pixel").await;
    let mut stream = ws_connect(server.addr, &server.host(), &cookie).await;
    let hello = ws_json(&mut stream).await;
    assert_eq!(hello["type"], json!("hello"));
    assert_eq!(hello["seq"], json!(0));

    server.core.publish(EventKind::SessionsChanged);
    let event = ws_json(&mut stream).await;
    assert_eq!(event["type"], json!("event"));
    assert_eq!(event["event"]["seq"], json!(1));
    assert_eq!(event["event"]["kind"]["type"], json!("sessionsChanged"));
}

#[tokio::test]
async fn revoke_drops_access_and_closes_the_socket() {
    let server = Server::start().await;
    let (device_id, cookie) = server.pair("Pixel").await;
    let mut stream = ws_connect(server.addr, &server.host(), &cookie).await;
    assert_eq!(ws_json(&mut stream).await["type"], json!("hello"));

    let revoked = call(
        server.addr,
        "POST",
        "/api/v1/devices/revoke",
        &server.host(),
        &cookie_header(&cookie),
        &json!({ "id": device_id }).to_string(),
    )
    .await;
    assert_eq!(revoked.json()["ok"], json!(true));
    assert_eq!(server.handle.devices().len(), 0);

    let (opcode, payload) = ws_frame(&mut stream).await;
    assert_eq!(opcode, 0x8, "revoked device gets a close frame");
    assert_eq!(payload, vec![0x03, 0xF0]);

    let after = call(
        server.addr,
        "GET",
        "/api/v1/status",
        &server.host(),
        &cookie_header(&cookie),
        "",
    )
    .await;
    assert_eq!(after.status, 401);
}

#[tokio::test]
async fn static_shell_and_healthz_need_no_auth() {
    let server = Server::start().await;
    let index = call(server.addr, "GET", "/", &server.host(), &[], "").await;
    assert_eq!(index.status, 200);
    let shell = call(server.addr, "GET", "/p", &server.host(), &[], "").await;
    assert_eq!(shell.status, 200);
    for name in [
        "app.js",
        "style.css",
        "manifest.webmanifest",
        "sw.js",
        "icon.svg",
    ] {
        let reply = call(
            server.addr,
            "GET",
            &format!("/assets/{name}"),
            &server.host(),
            &[],
            "",
        )
        .await;
        assert_eq!(reply.status, 200, "asset {name}");
        assert_ne!(reply.body.len(), 0);
    }
    let traversal = call(
        server.addr,
        "GET",
        "/assets/../Cargo.toml",
        &server.host(),
        &[],
        "",
    )
    .await;
    assert_eq!(traversal.status, 404);
    let health = call(server.addr, "GET", "/healthz", &server.host(), &[], "").await;
    assert_eq!(health.status, 200);
    assert_eq!(health.json()["ok"], json!(true));
}

#[tokio::test]
async fn shutdown_stops_the_listener() {
    let server = Server::start().await;
    let addr = server.addr;
    server.handle.shutdown().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn session_methods_accept_object_and_bare_string_params() {
    let server = Server::start().await;
    let (_device_id, cookie) = server.pair("Pixel").await;
    let rpc = |body: Value| {
        let host = server.host();
        let headers = cookie_header(&cookie);
        let addr = server.addr;
        async move {
            call(
                addr,
                "POST",
                "/api/v1/rpc",
                &host,
                &headers,
                &body.to_string(),
            )
            .await
            .json()
        }
    };

    let object = rpc(json!({ "method": "list_modes", "params": { "sessionId": "s1" } })).await;
    assert_eq!(object["ok"], json!(true), "{object}");
    assert_eq!(object["value"]["current"], json!("s1"));

    let bare = rpc(json!({ "method": "list_modes", "params": "s2" })).await;
    assert_eq!(bare["ok"], json!(true), "{bare}");
    assert_eq!(bare["value"]["current"], json!("s2"));

    let stop_bare = rpc(json!({ "method": "stop", "params": "s1" })).await;
    assert_eq!(stop_bare["ok"], json!(true), "{stop_bare}");

    let empty = rpc(json!({ "method": "list_modes", "params": "" })).await;
    assert_eq!(empty["ok"], json!(false));

    let no_params = rpc(json!({ "method": "list_sessions" })).await;
    assert_eq!(no_params["ok"], json!(true), "{no_params}");
    let agents = rpc(json!({ "method": "list_agents", "params": {} })).await;
    assert_eq!(agents["ok"], json!(true), "{agents}");
}

#[tokio::test]
async fn service_worker_is_allowed_to_control_root() {
    let server = Server::start().await;
    let reply = call(server.addr, "GET", "/assets/sw.js", &server.host(), &[], "").await;
    assert_eq!(reply.status, 200);
    assert!(reply
        .head
        .lines()
        .any(|line| line.eq_ignore_ascii_case("service-worker-allowed: /")));
}
