//! Windows: the NSIS installer puts Harness in `%LOCALAPPDATA%\Programs\Worse Harness`. A portable
//! copy installs by downloading the newest installer and running it silently after it quits.

use std::path::{Path, PathBuf};

/// Release asset of the installer.
pub(crate) const SETUP_ASSET: &str = "Worse-Harness-windows-x64-setup.exe";
pub(super) const PROGRESS: &str = "Downloading the installer…";
const APP_DIR: &str = "Worse Harness";

fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub(super) fn install_dir() -> Option<PathBuf> {
    Some(local_app_data()?.join("Programs").join(APP_DIR))
}

pub(super) fn is_installed(exe: &Path) -> bool {
    match (exe.parent(), install_dir()) {
        (Some(dir), Some(installed)) => super::same_dir(dir, &installed),
        _ => false,
    }
}

/// Where downloaded installers wait to run (like Antigravity's `antigravity-updater`).
pub(crate) fn updater_dir() -> PathBuf {
    local_app_data()
        .unwrap_or_else(std::env::temp_dir)
        .join("worse-harness-updater")
}

/// Installer arguments: like Antigravity, a small "Installing, please wait…" window without questions, then
/// the installed copy starts.
pub(crate) fn setup_args(update: bool) -> Vec<String> {
    let mut args = vec!["/LAUNCH".to_string()];
    if update {
        args.push("/UPDATE".to_string());
    }
    args
}

/// Downloads the newest installer; it runs after Harness quits.
pub(super) fn install() -> Result<(PathBuf, Vec<String>), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let setup = runtime.block_on(crate::release_update::download_setup())?;
    Ok((setup, setup_args(false)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn installed_copy_lives_in_programs() {
        let dir = install_dir().unwrap();
        assert!(dir.ends_with(r"Programs\Worse Harness"));
        assert!(is_installed(&dir.join("Worse-Harness.exe")));
        assert!(!is_installed(Path::new(
            r"C:\Users\x\Downloads\Worse-Harness.exe"
        )));
        assert_eq!(setup_args(true), ["/LAUNCH", "/UPDATE"]);
    }
}
