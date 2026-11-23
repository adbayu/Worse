//! Live update, phase 1 (Windows).
//!
//! `harness-slint.exe` cannot be overwritten while it runs, and its helper processes (memory MCP, delegate MCP,
//! computer MCP, API agent) are started from the same file. So the app starts itself as a *shadow copy* in
//! `<harness_home>/bin/harness-slint-<fingerprint>.exe`, keeps `HARNESS_SHADOW_SOURCE` pointing at the real exe, and
//! watches that file: when `cargo build` replaces it, the UI offers a restart that runs the newest build.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// Set in the shadow copy: the path of the real (build output) executable.
const SOURCE_ENV: &str = "HARNESS_SHADOW_SOURCE";
/// Set in the shadow copy: the source exe fingerprint seen at launch, so the watcher cannot miss a build that lands
/// between the launch and the first poll.
const FINGERPRINT_ENV: &str = "HARNESS_SHADOW_FINGERPRINT";
/// `HARNESS_NO_SHADOW=1` runs in place, as before.
const NO_SHADOW_ENV: &str = "HARNESS_NO_SHADOW";
const COPY_PREFIX: &str = "harness-slint-";
/// `.exe` on Windows, nothing on Linux / macOS.
const COPY_SUFFIX: &str = std::env::consts::EXE_SUFFIX;

fn env_present(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// Shadow state read once from the environment and then removed from it: every child (relaunch, terminal shells,
/// agent processes) would otherwise inherit the two variables.
#[derive(Debug, Default, PartialEq, Eq)]
struct ShadowEnv {
    source: Option<PathBuf>,
    fingerprint: Option<String>,
}

static SHADOW: std::sync::OnceLock<ShadowEnv> = std::sync::OnceLock::new();

fn shadow_env_from(source: Option<std::ffi::OsString>, fingerprint: Option<String>) -> ShadowEnv {
    ShadowEnv {
        source: source.filter(|v| !v.is_empty()).map(PathBuf::from),
        fingerprint: fingerprint.filter(|v| parse_fingerprint(v).is_some()),
    }
}

fn shadow_env() -> &'static ShadowEnv {
    SHADOW.get_or_init(|| {
        shadow_env_from(
            std::env::var_os(SOURCE_ENV),
            std::env::var(FINGERPRINT_ENV).ok(),
        )
    })
}

/// Captures `HARNESS_SHADOW_SOURCE` / `HARNESS_SHADOW_FINGERPRINT`, then drops them from the process environment.
/// Call while the process is still single-threaded (right after `shadow_launch()`), before any child is spawned.
pub fn take_shadow_env() {
    let _ = shadow_env();
    std::env::remove_var(SOURCE_ENV);
    std::env::remove_var(FINGERPRINT_ENV);
}

/// True when this process is the shadow copy started by `shadow_launch`.
#[must_use]
pub fn is_shadow() -> bool {
    shadow_env().source.is_some()
}

/// The executable a restart should run: the newest build (the source exe) for a shadow copy, else this process.
#[must_use]
pub fn launch_exe() -> PathBuf {
    shadow_env().source.clone().unwrap_or_else(|| {
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("harness-slint"))
    })
}

/// Directory holding the shadow copies.
fn bin_dir() -> PathBuf {
    crate::harness_home().join("bin")
}

/// `"<file len>-<mtime secs>"`: changes when the linker rewrites the executable.
fn fingerprint(len: u64, mtime_secs: Option<u64>) -> String {
    format!("{len}-{}", mtime_secs.unwrap_or_default())
}

/// Copy file name for a fingerprint.
fn copy_name(fingerprint: &str) -> String {
    format!("{COPY_PREFIX}{fingerprint}{COPY_SUFFIX}")
}

fn fingerprint_of(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    Ok(fingerprint(meta.len(), mtime))
}

/// `"<len>-<mtime>"` → `(len, mtime)`; anything else is not a fingerprint.
fn parse_fingerprint(value: &str) -> Option<(u64, u64)> {
    let (len, mtime) = value.trim().split_once('-')?;
    Some((len.parse().ok()?, mtime.parse().ok()?))
}

/// The fingerprint the watcher compares against: the one captured by `shadow_launch` (via `take_shadow_env`), or,
/// when it is missing or unparseable, the file as it is now.
fn baseline(source: &Path) -> Option<String> {
    shadow_env()
        .fingerprint
        .clone()
        .or_else(|| fingerprint_of(source).ok())
}

/// Starts the shadow copy and exits this process, or returns `None` to keep running in place.
#[must_use]
pub fn shadow_launch() -> Option<ExitCode> {
    if !cfg!(windows)
        || cfg!(feature = "automation")
        || env_present(NO_SHADOW_ENV)
        || env_present("HARNESS_UI_CORE")
        || env_present(SOURCE_ENV)
        // 安装版由安装程序更新（退出后静默安装），不需要影子副本，也不再在 ~/.harness/bin 留 exe。
        || crate::install::is_installed()
    {
        return None;
    }
    match start_shadow() {
        Ok(code) => Some(code),
        Err(error) => {
            eprintln!("harness-slint: shadow copy failed, running in place: {error}");
            None
        }
    }
}

fn start_shadow() -> Result<ExitCode, String> {
    let source = std::env::current_exe().map_err(|e| format!("current executable: {e}"))?;
    let fingerprint = fingerprint_of(&source)?;
    let dir = bin_dir();
    let copy = dir.join(copy_name(&fingerprint));
    if !copy.is_file() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let tmp = dir.join(format!("{}.part", copy_name(&fingerprint)));
        std::fs::copy(&source, &tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        if let Err(error) = std::fs::rename(&tmp, &copy) {
            // Another instance may have won the race: keep its copy, drop ours.
            let _ = std::fs::remove_file(&tmp);
            if !copy.is_file() {
                return Err(format!("{}: {error}", copy.display()));
            }
        }
    }
    let mut command = std::process::Command::new(&copy);
    command
        .args(std::env::args_os().skip(1))
        .env(SOURCE_ENV, &source)
        .env(FINGERPRINT_ENV, &fingerprint);
    if let Ok(cwd) = std::env::current_dir() {
        command.current_dir(cwd);
    }
    command
        .spawn()
        .map_err(|e| format!("{}: {e}", copy.display()))?;
    Ok(ExitCode::SUCCESS)
}

/// Removes the copies of older builds. A copy that is still running cannot be deleted on Windows; ignore that.
pub fn cleanup_old_copies() {
    let keep = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    cleanup_in(&bin_dir(), &keep);
}

fn cleanup_in(dir: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == keep || !name.starts_with(COPY_PREFIX) || !name.ends_with(COPY_SUFFIX) {
            continue;
        }
        let _ = std::fs::remove_file(entry.path());
    }
}

/// Polls the source exe; after a new build has landed twice in a row (the linker has finished), sets
/// `update-ready` on the window once and stops.
pub fn watch(window: slint::Weak<crate::ui::AppWindow>) {
    let Some(source) = shadow_env().source.clone() else {
        return;
    };
    std::thread::Builder::new()
        .name("harness-live-update".into())
        .spawn(move || {
            let Some(start) = baseline(&source) else {
                return;
            };
            let mut candidate: Option<String> = None;
            loop {
                std::thread::sleep(Duration::from_secs(3));
                let Ok(now) = fingerprint_of(&source) else {
                    // Mid-link the file can be missing: keep polling.
                    candidate = None;
                    continue;
                };
                if now == start {
                    candidate = None;
                    continue;
                }
                if candidate.as_deref() != Some(now.as_str()) {
                    candidate = Some(now);
                    continue;
                }
                let weak = window.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        window.set_update_ready(true);
                    }
                });
                return;
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::{
        cleanup_in, copy_name, fingerprint, parse_fingerprint, shadow_env_from, ShadowEnv,
    };

    #[test]
    fn shadow_env_from_filters_bad_values() {
        let none = shadow_env_from(Some("".into()), Some("junk".to_string()));
        assert_eq!(none, ShadowEnv::default());
        let some = shadow_env_from(
            Some(r"C:\build\harness-slint.exe".into()),
            Some("1024-1700000000".to_string()),
        );
        assert_eq!(some.source, Some(r"C:\build\harness-slint.exe".into()));
        assert_eq!(some.fingerprint.as_deref(), Some("1024-1700000000"));
    }

    #[test]
    fn fingerprint_parses_back() {
        assert_eq!(
            parse_fingerprint("1024-1700000000"),
            Some((1024, 1_700_000_000))
        );
        assert_eq!(parse_fingerprint("7-0"), Some((7, 0)));
        assert_eq!(
            parse_fingerprint("1024-1700000000").map(|(l, m)| fingerprint(l, Some(m))),
            Some("1024-1700000000".to_string())
        );
        for bad in [
            "",
            "1024",
            "abc-1700000000",
            "1024-",
            "-1",
            "1024-1-2",
            "1.5-2",
        ] {
            assert_eq!(parse_fingerprint(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn fingerprint_has_len_and_mtime() {
        assert_eq!(fingerprint(1024, Some(1_700_000_000)), "1024-1700000000");
        assert_eq!(fingerprint(7, None), "7-0");
    }

    #[test]
    fn copy_name_uses_the_fingerprint() {
        assert_eq!(
            copy_name("12-34"),
            format!("harness-slint-12-34{}", super::COPY_SUFFIX)
        );
    }

    #[test]
    fn cleanup_keeps_the_current_copy() {
        let dir = std::env::temp_dir().join(format!("harness-live-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let keep = "harness-slint-2-2.exe";
        for name in [
            keep,
            "harness-slint-1-1.exe",
            "harness-slint-3-3.exe",
            "other.exe",
        ] {
            std::fs::write(dir.join(name), b"x").expect("write");
        }
        cleanup_in(&dir, keep);
        assert!(dir.join(keep).is_file());
        assert!(!dir.join("harness-slint-1-1.exe").exists());
        assert!(!dir.join("harness-slint-3-3.exe").exists());
        assert!(dir.join("other.exe").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
