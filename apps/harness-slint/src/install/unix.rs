//! Linux and macOS: installs this copy for the current user, without root.
//! Linux: `~/.local/share/worse-harness/worse-harness`, a `.desktop` entry, an icon and a
//! `~/.local/bin/worse-harness` link. macOS: the app bundle copied to `/Applications` (or
//! `~/Applications` when `/Applications` is not writable).

use std::path::{Path, PathBuf};

pub(super) const PROGRESS: &str = "Installing…";

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())
}

#[cfg(target_os = "macos")]
const BUNDLE: &str = "Worse Harness.app";

#[cfg(target_os = "macos")]
fn bundle_of(exe: &Path) -> Option<&Path> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
}

#[cfg(target_os = "macos")]
pub(super) fn install_dir() -> Option<PathBuf> {
    let system = PathBuf::from("/Applications").join(BUNDLE);
    if system.is_dir() {
        return Some(system);
    }
    Some(home().ok()?.join("Applications").join(BUNDLE))
}

#[cfg(target_os = "macos")]
pub(super) fn is_installed(exe: &Path) -> bool {
    let user = home().ok().map(|h| h.join("Applications"));
    bundle_of(exe).and_then(Path::parent).is_some_and(|dir| {
        super::same_dir(dir, Path::new("/Applications"))
            || user.as_deref().is_some_and(|u| super::same_dir(dir, u))
    })
}

#[cfg(target_os = "macos")]
pub(super) fn install() -> Result<(PathBuf, Vec<String>), String> {
    let exe = crate::live_update::launch_exe();
    let bundle = bundle_of(&exe).ok_or("Harness is not running from an app bundle")?;
    let system = Path::new("/Applications");
    let writable = std::fs::metadata(system).is_ok_and(|m| !m.permissions().readonly())
        && std::fs::write(system.join(".worse-harness-check"), b"").is_ok();
    let _ = std::fs::remove_file(system.join(".worse-harness-check"));
    let dir = if writable {
        system.to_path_buf()
    } else {
        home()?.join("Applications")
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dest = dir.join(BUNDLE);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    let status = std::process::Command::new("/usr/bin/ditto")
        .arg(bundle)
        .arg(&dest)
        .status()
        .map_err(|e| format!("ditto: {e}"))?;
    if !status.success() {
        return Err(format!("ditto failed: {status}"));
    }
    let dest = dest.to_string_lossy().into_owned();
    Ok((PathBuf::from("/usr/bin/open"), vec!["-n".into(), dest]))
}

#[cfg(not(target_os = "macos"))]
const ICON: &[u8] = include_bytes!("../../../../crates/harness-ui/ui/brand/app-icon.png");

#[cfg(not(target_os = "macos"))]
pub(super) fn install_dir() -> Option<PathBuf> {
    Some(home().ok()?.join(".local/share/worse-harness"))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn is_installed(exe: &Path) -> bool {
    // A distro package (/usr, /opt) counts as installed too.
    let packaged = exe.starts_with("/usr") || exe.starts_with("/opt");
    packaged
        || match (exe.parent(), install_dir()) {
            (Some(dir), Some(installed)) => super::same_dir(dir, &installed),
            _ => false,
        }
}

#[cfg(not(target_os = "macos"))]
fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Worse Harness\nGenericName=Agent orchestrator\n\
         Comment=Run your coding agents side by side\nExec=\"{}\" %U\nIcon=worse-harness\n\
         Terminal=false\nCategories=Development;Utility;\nKeywords=harness;agent;ai;\n",
        exe.display()
    )
}

#[cfg(not(target_os = "macos"))]
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn install() -> Result<(PathBuf, Vec<String>), String> {
    use std::os::unix::fs::PermissionsExt;
    let home = home()?;
    let source = crate::live_update::launch_exe();
    let dir = install_dir().ok_or("no install directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let target = dir.join("worse-harness");
    if !super::same_dir(&source, &target) {
        // 先复制到临时文件再改名：正在运行的旧版本不会被写坏。
        let tmp = dir.join("worse-harness.part");
        std::fs::copy(&source, &tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        std::fs::rename(&tmp, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    let share = home.join(".local/share");
    write(
        &share.join("icons/hicolor/256x256/apps/worse-harness.png"),
        ICON,
    )?;
    let apps = share.join("applications");
    write(
        &apps.join("worse-harness.desktop"),
        desktop_entry(&target).as_bytes(),
    )?;
    let link = home.join(".local/bin/worse-harness");
    if let Some(bin) = link.parent() {
        let _ = std::fs::create_dir_all(bin);
    }
    let _ = std::fs::remove_file(&link);
    let _ = std::os::unix::fs::symlink(&target, &link);
    // Best effort: refresh the menu caches of desktops that keep one.
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&apps)
        .status();
    Ok((target, Vec::new()))
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn desktop_entry_points_at_the_installed_copy() {
        let entry = desktop_entry(Path::new(
            "/home/u/.local/share/worse-harness/worse-harness",
        ));
        assert!(entry.contains("Exec=\"/home/u/.local/share/worse-harness/worse-harness\" %U"));
        assert!(entry.contains("Icon=worse-harness"));
        assert!(is_installed(Path::new("/usr/bin/worse-harness")));
    }
}
