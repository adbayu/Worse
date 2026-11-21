//! Official release channels of Claude Code, Codex, agy and Node.js, mirrored from their install scripts:
//! Claude Code `downloads.claude.ai` manifest (SHA-256), agy auto-updater manifest (SHA-512), Codex the npm
//! platform package of `@openai/codex` (npm integrity), Node.js `nodejs.org/dist` (SHASUMS256).

use std::path::{Path, PathBuf};

use super::fetch::{self, Progress};

const CLAUDE_BASE: &str = "https://downloads.claude.ai/claude-code-releases";
const AGY_BASE: &str = "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app";
const NPM_REGISTRY: &str = "https://registry.npmjs.org";
const NODE_DIST: &str = "https://nodejs.org/dist";
const NODE_MIN_MAJOR: u64 = 22;

/// Whether Harness can download `cli` itself.
pub(crate) fn downloadable(cli: &str) -> bool {
    matches!(cli, "claude" | "codex" | "agy")
}

fn arm() -> bool {
    cfg!(target_arch = "aarch64")
}

fn musl() -> bool {
    cfg!(target_os = "linux")
        && (Path::new("/lib/libc.musl-x86_64.so.1").exists()
            || Path::new("/lib/libc.musl-aarch64.so.1").exists())
}

fn os() -> Result<&'static str, String> {
    if cfg!(windows) {
        Ok("win32")
    } else if cfg!(target_os = "macos") {
        Ok("darwin")
    } else if cfg!(target_os = "linux") {
        Ok("linux")
    } else {
        Err("this platform has no official build".to_owned())
    }
}

fn root() -> Result<PathBuf, String> {
    let root = super::root().ok_or("the Harness folder is not set")?;
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    Ok(root)
}

/// Downloads `cli` into `<home>/tools/<cli>` and returns the executable.
pub(crate) async fn download_cli(cli: &str, progress: Progress<'_>) -> Result<PathBuf, String> {
    match cli {
        "claude" => claude(progress).await,
        "codex" => codex(progress).await,
        "agy" => agy(progress).await,
        other => Err(format!("Harness cannot download `{other}`")),
    }
}

async fn claude(progress: Progress<'_>) -> Result<PathBuf, String> {
    let client = fetch::client()?;
    let version = fetch::text(&client, &format!("{CLAUDE_BASE}/latest")).await?;
    let version = version.trim();
    if !version.chars().next().is_some_and(|c| c.is_ascii_digit())
        || version.contains(char::is_whitespace)
    {
        return Err("downloads.claude.ai did not return a version (unreachable or not available in your region)".to_owned());
    }
    let arch = if arm() { "arm64" } else { "x64" };
    let suffix = if musl() { "-musl" } else { "" };
    let platform = format!("{}-{arch}{suffix}", os()?);
    let manifest = fetch::json(&client, &format!("{CLAUDE_BASE}/{version}/manifest.json")).await?;
    let checksum = manifest["platforms"][&platform]["checksum"]
        .as_str()
        .ok_or_else(|| format!("Claude Code {version} has no build for {platform}"))?
        .to_owned();
    let file = super::exe("claude");
    let label = format!("Claude Code {version}");
    let data = fetch::bytes(
        &client,
        &format!("{CLAUDE_BASE}/{version}/{platform}/{file}"),
        &label,
        progress,
    )
    .await?;
    fetch::verify(&label, &fetch::sha256_hex(&data), &checksum)?;
    progress(format!("Installing {label}"));
    let dir = fetch::place(&root()?, "claude", |tmp| {
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin)
            .map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
        let path = bin.join(&file);
        std::fs::write(&path, &data)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        fetch::make_executable(&path)
    })?;
    Ok(dir.join("bin").join(file))
}

async fn agy(progress: Progress<'_>) -> Result<PathBuf, String> {
    let client = fetch::client()?;
    let os = match os()? {
        "win32" => "windows",
        other => other,
    };
    let arch = if arm() { "arm64" } else { "amd64" };
    let suffix = if musl() { "_musl" } else { "" };
    let platform = format!("{os}_{arch}{suffix}");
    let manifest = fetch::json(&client, &format!("{AGY_BASE}/manifests/{platform}.json")).await?;
    let field = |key: &str| manifest[key].as_str().unwrap_or_default().to_owned();
    let (version, url, sha512) = (field("version"), field("url"), field("sha512"));
    if url.is_empty() {
        return Err(format!(
            "the agy release manifest for {platform} has no download"
        ));
    }
    let label = format!("agy {version}");
    let data = fetch::bytes(&client, &url, &label, progress).await?;
    fetch::verify(&label, &fetch::sha512_hex(&data), &sha512)?;
    progress(format!("Installing {label}"));
    let file = super::exe("agy");
    let dir = fetch::place(&root()?, "agy", |tmp| {
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin)
            .map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
        let path = bin.join(&file);
        if url.contains(".tar.gz") {
            let found = fetch::untar_gz(&data, &bin, &|p: &Path| {
                (p.file_name().is_some_and(|n| n == "antigravity")).then(|| PathBuf::from(&file))
            })?;
            if found == 0 {
                return Err("the agy archive has no `antigravity` binary".to_owned());
            }
        } else {
            std::fs::write(&path, &data)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
        fetch::make_executable(&path)
    })?;
    Ok(dir.join("bin").join(file))
}

async fn codex(progress: Progress<'_>) -> Result<PathBuf, String> {
    let client = fetch::client()?;
    let latest = fetch::json(&client, &format!("{NPM_REGISTRY}/@openai/codex/latest")).await?;
    let version = latest["version"]
        .as_str()
        .ok_or("npm has no Codex version")?
        .to_owned();
    let platform = format!("{}-{}", os()?, if arm() { "arm64" } else { "x64" });
    let meta = fetch::json(
        &client,
        &format!("{NPM_REGISTRY}/@openai/codex/{version}-{platform}"),
    )
    .await?;
    let tarball = meta["dist"]["tarball"]
        .as_str()
        .ok_or_else(|| format!("Codex {version} has no build for {platform}"))?;
    let integrity = meta["dist"]["integrity"].as_str().unwrap_or_default();
    let label = format!("Codex {version}");
    let data = fetch::bytes(&client, tarball, &label, progress).await?;
    if !fetch::npm_integrity_ok(&data, integrity) {
        return Err(format!(
            "checksum mismatch for {label}; nothing was installed"
        ));
    }
    progress(format!("Installing {label}"));
    let file = super::exe("codex");
    let dir = fetch::place(&root()?, "codex", |tmp| {
        fetch::untar_gz(&data, tmp, &|p: &Path| {
            let mut parts = p.components();
            let vendored = parts.next().is_some_and(|c| c.as_os_str() == "package")
                && parts.next().is_some_and(|c| c.as_os_str() == "vendor");
            if vendored {
                fetch::strip(p, 3)
            } else {
                None
            }
        })?;
        let path = tmp.join("bin").join(&file);
        if !path.is_file() {
            return Err(format!("the Codex package has no bin/{file}"));
        }
        for name in ["codex", "rg"] {
            for sub in ["bin", "codex-path"] {
                let p = tmp.join(sub).join(name);
                if p.is_file() {
                    fetch::make_executable(&p)?;
                }
            }
        }
        Ok(())
    })?;
    Ok(dir.join("bin").join(file))
}

/// Downloads the newest Node.js LTS (≥ 22) into `<home>/tools/node` and returns its npm.
pub(crate) async fn download_node(progress: Progress<'_>) -> Result<PathBuf, String> {
    let client = fetch::client()?;
    let index = fetch::json(&client, &format!("{NODE_DIST}/index.json")).await?;
    let version = index
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| !r["lts"].is_boolean() || r["lts"].as_bool() == Some(true))
        .filter_map(|r| r["version"].as_str())
        .find(|v| {
            v.trim_start_matches('v')
                .split('.')
                .next()
                .and_then(|m| m.parse::<u64>().ok())
                .is_some_and(|m| m >= NODE_MIN_MAJOR)
        })
        .ok_or("nodejs.org lists no LTS release")?
        .to_owned();
    let arch = if arm() { "arm64" } else { "x64" };
    let (os, ext) = match os()? {
        "win32" => ("win", "zip"),
        "darwin" => ("darwin", "tar.gz"),
        _ => ("linux", "tar.gz"),
    };
    let name = format!("node-{version}-{os}-{arch}.{ext}");
    let sums = fetch::text(&client, &format!("{NODE_DIST}/{version}/SHASUMS256.txt")).await?;
    let expected = sums
        .lines()
        .find_map(|l| {
            l.split_once("  ")
                .filter(|(_, f)| f.trim() == name)
                .map(|(h, _)| h.to_owned())
        })
        .ok_or_else(|| format!("nodejs.org has no checksum for {name}"))?;
    let label = format!("Node.js {version}");
    let data = fetch::bytes(
        &client,
        &format!("{NODE_DIST}/{version}/{name}"),
        &label,
        progress,
    )
    .await?;
    fetch::verify(&label, &fetch::sha256_hex(&data), &expected)?;
    progress(format!("Installing {label}"));
    fetch::place(&root()?, "node", |tmp| {
        let top = &|p: &Path| fetch::strip(p, 1);
        if ext == "zip" {
            fetch::unzip(&data, tmp, top)
        } else {
            fetch::untar_gz(&data, tmp, top)
        }
        .map(|_| ())
    })?;
    super::harness_npm().ok_or_else(|| "the Node.js download has no npm".to_owned())
}
