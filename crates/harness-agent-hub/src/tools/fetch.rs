//! Download, checksum and unpack helpers for the Harness tool installs.

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use sha2::Digest as _;

pub(super) type Progress<'a> = &'a (dyn Fn(String) + Send + Sync);

pub(super) fn client() -> Result<reqwest::Client, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("cannot start download: {e}"))
}

pub(super) async fn text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url}: HTTP {}", response.status()));
    }
    response.text().await.map_err(|e| format!("{url}: {e}"))
}

pub(super) async fn json(client: &reqwest::Client, url: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(&text(client, url).await?)
        .map_err(|e| format!("{url}: unexpected data: {e}"))
}

/// Downloads `url`, reporting progress every 10 %.
pub(super) async fn bytes(
    client: &reqwest::Client,
    url: &str,
    label: &str,
    progress: Progress<'_>,
) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download of {label} failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "download of {label} failed: HTTP {}",
            response.status()
        ));
    }
    let total = response.content_length().unwrap_or(0);
    let mut out = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
    let mut shown = 0;
    progress(format!("Downloading {label}"));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("download of {label} failed: {e}"))?
    {
        out.extend_from_slice(&chunk);
        if let Some(percent) = (out.len() as u64 * 100).checked_div(total) {
            if percent >= shown + 10 {
                shown = percent - percent % 10;
                progress(format!("Downloading {label}: {shown}%"));
            }
        }
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    hex(&sha2::Sha256::digest(bytes))
}

pub(super) fn sha512_hex(bytes: &[u8]) -> String {
    hex(&sha2::Sha512::digest(bytes))
}

/// Checks an npm `dist.integrity` (`sha512-<base64>`).
pub(super) fn npm_integrity_ok(bytes: &[u8], integrity: &str) -> bool {
    integrity
        .split_whitespace()
        .filter_map(|part| part.strip_prefix("sha512-"))
        .filter_map(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .any(|want| want == sha2::Sha512::digest(bytes).as_slice())
}

pub(super) fn verify(label: &str, actual: &str, expected: &str) -> Result<(), String> {
    if expected.is_empty() || !actual.eq_ignore_ascii_case(expected.trim()) {
        return Err(format!(
            "checksum mismatch for {label}; nothing was installed"
        ));
    }
    Ok(())
}

/// A relative path with only normal components (no `..`, root or drive).
fn safe(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Unpacks the `.tar.gz` entries `map` keeps (archive path → path inside `dest`).
pub(super) fn untar_gz(
    bytes: &[u8],
    dest: &Path,
    map: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Result<usize, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut count = 0;
    for entry in archive
        .entries()
        .map_err(|e| format!("cannot read archive: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("cannot read archive: {e}"))?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink()) {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| format!("cannot read archive: {e}"))?
            .into_owned();
        let Some(target) = map(&path).and_then(|p| safe(&p)) else {
            continue;
        };
        let target = dest.join(target);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        entry
            .unpack(&target)
            .map_err(|e| format!("cannot extract {}: {e}", target.display()))?;
        count += 1;
    }
    Ok(count)
}

/// Unpacks a `.zip`, mapping paths like [`untar_gz`].
pub(super) fn unzip(
    bytes: &[u8],
    dest: &Path,
    map: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Result<usize, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("cannot read archive: {e}"))?;
    let mut count = 0;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("cannot read archive: {e}"))?;
        let Some(name) = file.enclosed_name() else {
            continue;
        };
        let Some(target) = map(&name).and_then(|p| safe(&p)) else {
            continue;
        };
        let target = dest.join(target);
        if file.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| format!("cannot create {}: {e}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data)
            .map_err(|e| format!("cannot extract {}: {e}", target.display()))?;
        std::fs::write(&target, data)
            .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        count += 1;
    }
    Ok(count)
}

/// Archive path without its first `n` components.
pub(super) fn strip(path: &Path, n: usize) -> Option<PathBuf> {
    let rest: PathBuf = path.components().skip(n).collect();
    (!rest.as_os_str().is_empty()).then_some(rest)
}

#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
pub(super) fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot make {} executable: {e}", path.display()))?;
    }
    let _ = path;
    Ok(())
}

/// Fills `<tools>/<name>.tmp` with `fill`, then replaces `<tools>/<name>` with it.
pub(super) fn place(
    tools: &Path,
    name: &str,
    fill: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let tmp = tools.join(format!("{name}.tmp"));
    let target = tools.join(name);
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)
            .map_err(|e| format!("cannot clear {}: {e}", tmp.display()))?;
    }
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let result = fill(&tmp).and_then(|()| {
        if target.exists() {
            std::fs::remove_dir_all(&target).map_err(|e| {
                format!(
                    "cannot replace {} (is it still running?): {e}",
                    target.display()
                )
            })?;
        }
        std::fs::rename(&tmp, &target)
            .map_err(|e| format!("cannot move into {}: {e}", target.display()))
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    result.map(|()| target)
}

#[cfg(test)]
mod tests {
    use super::{npm_integrity_ok, safe, strip};
    use std::path::Path;

    #[test]
    fn archive_paths_cannot_escape() {
        assert!(safe(Path::new("../x")).is_none());
        assert!(safe(Path::new("/etc/x")).is_none());
        assert_eq!(safe(Path::new("a/b")).as_deref(), Some(Path::new("a/b")));
        assert_eq!(
            strip(Path::new("node-v1/bin/node"), 1).as_deref(),
            Some(Path::new("bin/node"))
        );
        assert!(strip(Path::new("node-v1"), 1).is_none());
    }

    #[test]
    fn npm_integrity_is_checked() {
        // sha512("") in base64.
        let empty = "sha512-z4PhNX7vuL3xVChQ1m2AB9Yg5AULVxXcg/SpIdNs6c5H0NE8XYXysP+DGNKHfuwvY7kxvUdBeoGlODJ6+SfaPg==";
        assert!(npm_integrity_ok(b"", empty));
        assert!(!npm_integrity_ok(b"x", empty));
    }
}
