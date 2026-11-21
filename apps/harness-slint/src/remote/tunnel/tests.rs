#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use std::time::Instant;

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "harness-tunnel-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Writes a `cmd` stub; `body` runs inside `@echo off`.
/// A fake tunnel binary. `body` is batch; on Unix it becomes an executable `sh` script (`ping -n 30` → `sleep 30`,
/// `exit /b N` → `exit N`).
fn stub(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let path = dir.join(name);
        std::fs::write(&path, format!("@echo off\r\n{body}\r\n")).expect("write stub");
        path
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name).with_extension("sh");
        let script = body
            .replace("\r\n", "\n")
            .replace("ping -n 30 127.0.0.1 >nul", "sleep 30")
            .replace("exit /b ", "exit ");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
        path
    }
}

fn wait_for(tunnel: &Tunnel, want: impl Fn(&State) -> bool, timeout: Duration) -> State {
    let deadline = Instant::now() + timeout;
    loop {
        let state = tunnel.state();
        if want(&state) || Instant::now() >= deadline {
            return state;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn first_url_takes_the_tunnel_link() {
    assert_eq!(
        first_url("INF +--------------------------------------------------------------------------------------------+"),
        None
    );
    assert_eq!(
        first_url("2024-01-01 INF |  https://harness.trycloudflare.com  |").as_deref(),
        Some("https://harness.trycloudflare.com")
    );
    // Trailing punctuation and quotes are not part of the URL.
    assert_eq!(
        first_url("Visit https://abc-def.trycloudflare.com, then pair.").as_deref(),
        Some("https://abc-def.trycloudflare.com")
    );
    assert_eq!(
        first_url(r#"{"url":"https://sub.example.com"}"#).as_deref(),
        Some("https://sub.example.com")
    );
    // Text without a dotted host is not a tunnel URL.
    assert_eq!(first_url("see https://localhost"), None);
    assert_eq!(first_url("http://plain.example.com"), None);
}

#[test]
fn resolve_bin_appends_exe_only_when_needed() {
    if !cfg!(windows) {
        assert_eq!(
            resolve_bin("/usr/bin/cloudflared"),
            PathBuf::from("/usr/bin/cloudflared")
        );
        return;
    }
    // Extensionless and not a file → `.exe` (e.g. `C:\tools\cftunnel`).
    assert_eq!(
        resolve_bin(r"D:\harness-missing-cftunnel"),
        PathBuf::from(r"D:\harness-missing-cftunnel.exe")
    );
    // Already has an extension → unchanged.
    assert_eq!(
        resolve_bin(r"C:\tools\cftunnel.exe"),
        PathBuf::from(r"C:\tools\cftunnel.exe")
    );
    // An existing extensionless file is used as it is.
    let dir = temp_dir("resolve");
    let plain = dir.join("tunnel");
    std::fs::write(&plain, b"x").expect("write");
    assert_eq!(resolve_bin(&plain.to_string_lossy()), plain);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stub_url_line_goes_up_and_stop_returns_off() {
    let dir = temp_dir("up");
    let path = stub(
        &dir,
        "fake-cftunnel.cmd",
        "echo starting tunnel\r\necho https://stub-sub.trycloudflare.com is up\r\nping -n 30 127.0.0.1 >nul",
    );
    let tunnel = Tunnel::new();
    let cfg = TunnelConfig::new(&path.to_string_lossy(), 7878, "stub-sub", "");
    tunnel.start(&cfg).expect("start");
    let state = wait_for(&tunnel, State::is_running, Duration::from_secs(5));
    assert!(state.is_running(), "{state:?}");
    let state = wait_for(
        &tunnel,
        |s| matches!(s, State::Up { .. }),
        Duration::from_secs(10),
    );
    assert_eq!(
        state,
        State::Up {
            url: "https://stub-sub.trycloudflare.com".into()
        },
        "log: {:?}",
        tunnel.log()
    );
    assert!(tunnel.log().len() <= MAX_LOG);
    tunnel.stop();
    assert_eq!(tunnel.state(), State::Off);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn child_death_is_an_error_state() {
    let dir = temp_dir("death");
    let path = stub(&dir, "dying.cmd", "echo no url here\r\nexit /b 3");
    let tunnel = Tunnel::new();
    tunnel
        .start(&TunnelConfig::new(&path.to_string_lossy(), 7878, "", ""))
        .expect("start");
    let state = wait_for(
        &tunnel,
        |s| matches!(s, State::Error { .. }),
        Duration::from_secs(10),
    );
    assert!(matches!(state, State::Error { .. }), "{state:?}");
    tunnel.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_url_times_out_into_the_configured_fallback() {
    let dir = temp_dir("timeout");
    let path = stub(&dir, "silent.cmd", "ping -n 30 127.0.0.1 >nul");
    let tunnel = Tunnel::new();
    let mut cfg = TunnelConfig::new(&path.to_string_lossy(), 7878, "harness", "");
    cfg.timeout = Duration::from_millis(600);
    tunnel.start(&cfg).expect("start");
    let state = wait_for(
        &tunnel,
        |s| !matches!(s, State::Starting),
        Duration::from_secs(10),
    );
    // No zone configured: that is an error, not a silent `Starting` forever.
    assert!(matches!(state, State::Error { .. }), "{state:?}");
    tunnel.stop();

    let path = stub(&dir, "silent2.cmd", "ping -n 30 127.0.0.1 >nul");
    let mut cfg = TunnelConfig::new(&path.to_string_lossy(), 7878, "harness", "example.com");
    cfg.timeout = Duration::from_millis(600);
    tunnel.start(&cfg).expect("start");
    let state = wait_for(
        &tunnel,
        |s| !matches!(s, State::Starting),
        Duration::from_secs(10),
    );
    assert_eq!(
        state,
        State::Up {
            url: "https://harness.example.com".into()
        }
    );
    tunnel.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_binary_reports_an_error() {
    let tunnel = Tunnel::new();
    let missing = std::env::temp_dir().join("harness-no-such-tunnel-binary");
    let started = tunnel.start(&TunnelConfig::new(&missing.to_string_lossy(), 7878, "", ""));
    assert!(started.is_err());
    assert!(matches!(tunnel.state(), State::Error { .. }));
}
