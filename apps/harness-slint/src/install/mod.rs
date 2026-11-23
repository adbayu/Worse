//! Installed vs portable copies. Like the Antigravity desktop app, Harness lives in the per-user app
//! location of each OS, so system search and the app menu find it and updates replace it in place:
//!
//! - Windows: `%LOCALAPPDATA%\Programs\Worse Harness\Worse-Harness.exe`, put there by the NSIS
//!   installer (`packaging/windows/installer.nsi`) with Start menu / desktop shortcuts and an
//!   Add/Remove Programs entry. Updates run the newer installer silently after the app quits.
//! - Linux: `~/.local/share/worse-harness/worse-harness` plus a `.desktop` entry, an icon and
//!   `~/.local/bin/worse-harness` (`worse-harness --install`, also run by the tarball's `install.sh`).
//! - macOS: `/Applications/Worse Harness.app` (or `~/Applications`), from the `.dmg` or the in-app
//!   button.
//!
//! A copy anywhere else is *portable*: Settings › General offers to install it. Dev builds (under a
//! `target` directory) are neither and are left alone.

#[cfg(not(windows))]
mod unix;
#[cfg(windows)]
pub(crate) mod win;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

#[cfg(not(windows))]
use unix as platform;
#[cfg(windows)]
use win as platform;

use slint::ComponentHandle;

use crate::ui;

/// Command line flag: install this copy (Linux / macOS) and exit.
pub(crate) const FLAG_INSTALL: &str = "--install";

/// What runs after the event loop ends: the installer, or the freshly installed copy.
static PENDING: Mutex<Option<(PathBuf, Vec<String>)>> = Mutex::new(None);

pub(crate) fn set_pending(program: PathBuf, args: Vec<String>) {
    if let Ok(mut slot) = PENDING.lock() {
        *slot = Some((program, args));
    }
}

/// Starts the pending installer / installed copy in `workspace`. False when nothing is pending.
pub(crate) fn run_pending(workspace: &Path) -> bool {
    let Some((program, args)) = PENDING.lock().ok().and_then(|mut slot| slot.take()) else {
        return false;
    };
    if let Err(error) = std::process::Command::new(&program)
        .args(&args)
        .current_dir(workspace)
        .env("HARNESS_WORKSPACE", workspace)
        .spawn()
    {
        eprintln!("harness-slint: cannot start {}: {error}", program.display());
    }
    true
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// True when this process runs the installed copy.
#[must_use]
pub(crate) fn is_installed() -> bool {
    platform::is_installed(&crate::live_update::launch_exe())
}

/// Deletes `<exe>.old`, which the Windows installer leaves when it replaced a running exe.
pub(crate) fn cleanup_replaced_exe() {
    let exe = crate::live_update::launch_exe();
    if let Some(name) = exe.file_name() {
        let mut old = name.to_os_string();
        old.push(".old");
        let _ = std::fs::remove_file(exe.with_file_name(old));
    }
}

/// The directory the installed copy lives in (never a workspace).
#[must_use]
pub(crate) fn install_dir() -> Option<PathBuf> {
    platform::install_dir()
}

/// A copy outside the install location that is not a dev build.
#[must_use]
pub(crate) fn is_portable() -> bool {
    let exe = crate::live_update::launch_exe();
    !crate::release_update::is_dev_build(&exe) && !platform::is_installed(&exe)
}

/// `harness-slint --install`: installs this copy for the current user, without the UI.
// 修复：新版 clippy 要求返回 ExitCode 的函数上的 #[must_use] 带说明文字（CI 报 double_must_use）。
#[must_use = "return the exit code from main"]
pub(crate) fn run_cli() -> ExitCode {
    if cfg!(windows) {
        eprintln!("On Windows, run Worse-Harness-windows-x64-setup.exe to install Harness.");
        return ExitCode::FAILURE;
    }
    match platform::install() {
        Ok((program, args)) => {
            println!(
                "Installed Worse Harness. Start it from the app menu, or: {} {}",
                program.display(),
                args.join(" ")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Install failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Settings › General "Install Harness": installs, then quits so the installed copy takes over.
pub(crate) fn install_ui(window: &ui::AppWindow, workspace: &Path) {
    let state = window.global::<ui::InstallUi>();
    state.set_portable(is_portable());
    let weak = window.as_weak();
    let dir = workspace.to_path_buf();
    state.on_install(move || {
        if let Some(window) = weak.upgrade() {
            let state = window.global::<ui::InstallUi>();
            state.set_busy(true);
            state.set_status(platform::PROGRESS.into());
        }
        let weak = weak.clone();
        let dir = dir.clone();
        std::thread::spawn(move || {
            let result = platform::install();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(window) = weak.upgrade() else {
                    return;
                };
                let state = window.global::<ui::InstallUi>();
                match result {
                    Ok((program, args)) => {
                        set_pending(program, args);
                        if let Ok(mut slot) = crate::startup::RELAUNCH.lock() {
                            *slot = Some(dir);
                        }
                        state.set_status("Installed. Restarting Harness…".into());
                        let _ = slint::quit_event_loop();
                    }
                    Err(error) => {
                        state.set_busy(false);
                        state.set_status(format!("Install failed: {error}").into());
                    }
                }
            });
        });
    });
}
