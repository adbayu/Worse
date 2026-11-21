use super::*;

#[tokio::test]
async fn allowlist_rejects_shutdown_and_roundtrips_list_sessions() {
    let server = Server::start().await;
    let (_device_id, cookie) = server.pair("Pixel").await;
    let blocked = call(
        server.addr,
        "POST",
        "/api/v1/rpc",
        &server.host(),
        &cookie_header(&cookie),
        &json!({ "method": "shutdown" }).to_string(),
    )
    .await;
    assert_eq!(blocked.status, 200);
    let value = blocked.json();
    assert_eq!(value["ok"], json!(false));
    assert!(value["error"]
        .as_str()
        .unwrap()
        .contains("method not allowed"));

    let listed = call(
        server.addr,
        "POST",
        "/api/v1/rpc",
        &server.host(),
        &cookie_header(&cookie),
        &json!({ "method": "list_sessions", "params": {} }).to_string(),
    )
    .await;
    assert_eq!(listed.status, 200);
    let value = listed.json();
    assert_eq!(value["ok"], json!(true));
    assert_eq!(value["value"][0]["id"], json!("s1"));
    assert_eq!(value["value"][0]["title"], json!("Test session"));

    let stopped = call(
        server.addr,
        "POST",
        "/api/v1/rpc",
        &server.host(),
        &cookie_header(&cookie),
        &json!({ "method": "stop", "params": { "sessionId": "s1" } }).to_string(),
    )
    .await;
    assert_eq!(stopped.json()["ok"], json!(true));
}

#[tokio::test]
async fn rpc_body_over_sixteen_megabytes_is_413() {
    let server = Server::start().await;
    let (_device_id, cookie) = server.pair("Pixel").await;
    let reply = call_declared(
        server.addr,
        "/api/v1/rpc",
        &server.host(),
        &cookie_header(&cookie),
        16 * 1024 * 1024 + 16,
    )
    .await;
    assert_eq!(reply.status, 413);
}

#[tokio::test]
async fn rpc_accepts_a_three_megabyte_body() {
    let server = Server::start().await;
    let (_device_id, cookie) = server.pair("Pixel").await;
    let image = "A".repeat(3 * 1024 * 1024);
    let body = json!({
        "method": "send_message",
        "params": {
            "sessionId": "s1",
            "text": "look at this",
            "context": [{ "type": "image", "path": null, "dataBase64": image, "mime": "image/jpeg" }],
        },
    })
    .to_string();
    assert!(body.len() > 3 * 1024 * 1024);
    let reply = call(
        server.addr,
        "POST",
        "/api/v1/rpc",
        &server.host(),
        &cookie_header(&cookie),
        &body,
    )
    .await;
    assert_eq!(reply.status, 200, "3 MB rpc body must be accepted");
    // The stub core rejects `send_message`; reaching it proves the body was parsed.
    assert!(reply.json()["error"]
        .as_str()
        .is_some_and(|error| error.contains("send_message")));
}

#[tokio::test]
async fn upload_saves_a_sanitized_file_inside_remote_uploads() {
    let server = Server::start().await;
    let (_device_id, cookie) = server.pair("Pixel").await;
    let reply = call(
        server.addr,
        "POST",
        "/api/v1/upload",
        &server.host(),
        &[
            ("X-File-Name", "../../evil.txt".to_owned()),
            cookie_header(&cookie)[0].clone(),
        ],
        "mobile attachment",
    )
    .await;
    assert_eq!(reply.status, 200, "{:?}", reply.text());
    let value = reply.json();
    assert_eq!(value["ok"], json!(true), "{value}");
    let path = PathBuf::from(value["path"].as_str().unwrap());
    assert!(path.is_absolute(), "path must be absolute: {path:?}");
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.ends_with("-evil.txt") && !name.contains(".."),
        "sanitized name expected, got {name}"
    );
    let parent = path.parent().unwrap();
    assert_eq!(
        parent.file_name().unwrap().to_string_lossy().len(),
        8,
        "uploads are grouped by yyyymmdd"
    );
    assert!(parent.parent().unwrap().ends_with("remote-uploads"));
    assert!(path.starts_with(&server.dir), "must stay inside state_dir");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mobile attachment");
    assert!(!server.dir.join("evil.txt").exists());
    assert!(!server.dir.join("..").join("evil.txt").exists());
}

#[tokio::test]
async fn upload_requires_auth() {
    let server = Server::start().await;
    let reply = call(
        server.addr,
        "POST",
        "/api/v1/upload",
        &server.host(),
        &[("X-File-Name", "note.txt".to_owned())],
        "secret",
    )
    .await;
    assert_eq!(reply.status, 401);
    assert_eq!(reply.body.len(), 0);
}

#[tokio::test]
async fn upload_over_twenty_five_megabytes_is_413() {
    let server = Server::start().await;
    let (_device_id, cookie) = server.pair("Pixel").await;
    let reply = call_declared(
        server.addr,
        "/api/v1/upload",
        &server.host(),
        &[
            ("X-File-Name", "big.bin".to_owned()),
            cookie_header(&cookie)[0].clone(),
        ],
        25 * 1024 * 1024 + 16,
    )
    .await;
    assert_eq!(reply.status, 413);
}
