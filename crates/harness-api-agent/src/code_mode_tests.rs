//! End-to-end tests of code mode against a fake host, so they need no agent, file system or
//! permissions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::code_mode_engine::{self, HostRequest, Script};

fn run_with<F>(code: &str, limit: Duration, answer: F) -> Value
where
    F: Fn(&HostRequest) -> String + Send + 'static,
{
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let interrupted = Arc::new(AtomicBool::new(false));
    let script = Script::new(
        code.to_owned(),
        request_tx,
        reply_rx,
        Arc::clone(&interrupted),
        limit,
    );
    let worker = std::thread::spawn(move || code_mode_engine::run(script));
    let watchdog = code_mode_engine::watch(Arc::clone(&interrupted), limit);
    while let Some(request) = request_rx.blocking_recv() {
        if reply_tx.send(answer(&request)).is_err() {
            break;
        }
    }
    let text = worker
        .join()
        .unwrap_or_else(|_| "{\"error\": \"panicked\"}".to_owned());
    interrupted.store(true, Ordering::SeqCst);
    let _ = watchdog.join();
    serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text}))
}

#[test]
fn script_calls_host_and_returns_result_with_logs() {
    let payload = run_with(
        "async () => { console.log(\"start\", 2); const r = await codemode.read_file({path: \"a.rs\"}); \
         return r.toUpperCase(); }",
        Duration::from_secs(20),
        |request| {
            assert_eq!(request.name, "read_file");
            assert_eq!(request.args, "{\"path\":\"a.rs\"}");
            "hello".to_owned()
        },
    );
    assert_eq!(payload["result"], json!("HELLO"), "{payload}");
    assert_eq!(payload["logs"], json!(["log: start 2"]), "{payload}");
}

#[test]
fn bare_body_is_wrapped_and_still_runs() {
    let payload = run_with(
        "const r = await codemode.list_dir({path: \"src\"});\nreturn r.length;",
        Duration::from_secs(20),
        |_| "abcd".to_owned(),
    );
    assert_eq!(payload["result"], json!(4), "{payload}");
}

#[test]
fn rejected_call_reaches_the_script() {
    let payload = run_with(
        "async () => { try { await codemode.read_file({path: \"x\"}); } catch (e) { return \"caught: \" + e; } }",
        Duration::from_secs(20),
        |_| "Error: the user denied this action".to_owned(),
    );
    assert_eq!(
        payload["result"],
        json!("caught: Error: the user denied this action"),
        "{payload}"
    );
}

#[test]
fn timeout_stops_an_endless_loop() {
    let start = Instant::now();
    let payload = run_with(
        "async () => { while (true) {} }",
        Duration::from_millis(300),
        |_| String::new(),
    );
    assert!(
        payload["error"]
            .as_str()
            .unwrap_or_default()
            .contains("code mode"),
        "{payload}"
    );
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn host_call_cap_stops_the_script() {
    let payload = run_with(
        "async () => { for (let i = 0; i < 100; i++) { await codemode.read_file({path: \"a\"}); } return \"done\"; }",
        Duration::from_secs(20),
        |_| "ok".to_owned(),
    );
    let error = payload["error"].as_str().unwrap_or_default();
    assert!(error.contains("at most 50 tool calls"), "{error}");
}

#[test]
fn thrown_error_is_reported_with_logs() {
    let payload = run_with(
        "async () => { console.warn(\"careful\"); throw new Error(\"boom\"); }",
        Duration::from_secs(20),
        |_| String::new(),
    );
    let error = payload["error"].as_str().unwrap_or_default();
    assert!(error.contains("boom"), "{payload}");
    assert_eq!(payload["logs"], json!(["warn: careful"]), "{payload}");
}
