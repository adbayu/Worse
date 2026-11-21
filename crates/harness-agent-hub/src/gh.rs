use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LIMIT: Duration = Duration::from_secs(60);
const CAP: usize = 8 * 1024 * 1024;

pub(crate) struct ToolOut {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

impl ToolOut {
    pub(crate) fn into_result(self, what: &str) -> Result<String, String> {
        if self.ok {
            Ok(self.stdout)
        } else {
            let detail = if self.stderr.is_empty() {
                self.stdout.trim().to_owned()
            } else {
                self.stderr
            };
            Err(format!("{what}: {}", first_lines(&detail)))
        }
    }
}

fn first_lines(text: &str) -> String {
    let mut out: String = text.lines().take(4).collect::<Vec<_>>().join(" ");
    if out.chars().count() > 400 {
        out = out.chars().take(400).collect();
    }
    out
}

static TOOLS: OnceLock<PathBuf> = OnceLock::new();
static INSTALLING: AtomicBool = AtomicBool::new(false);

const RELEASE_URL: &str = "https://api.github.com/repos/cli/cli/releases/latest";
const LATEST_PAGE: &str = "https://github.com/cli/cli/releases/latest";
const DOWNLOAD_BASE: &str = "https://github.com/cli/cli/releases/download";
const INSTALL_LIMIT: Duration = Duration::from_secs(600);

pub(crate) fn set_home(home: &Path) {
    let _ = TOOLS.set(home.join("tools"));
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "gh.exe"
    } else {
        "gh"
    }
}

fn bundled_path() -> Option<PathBuf> {
    TOOLS
        .get()
        .map(|tools| tools.join("gh").join("bin").join(exe_name()))
}

pub(crate) fn resolve_gh() -> Option<(PathBuf, bool)> {
    if let Some(value) = std::env::var_os("HARNESS_GH").filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        return path.is_file().then_some((path, false));
    }
    if let Some(path) = crate::agents::resolve_program("gh") {
        return Some((path, false));
    }
    bundled_path()
        .filter(|path| path.is_file())
        .map(|path| (path, true))
}

pub(crate) fn gh_program() -> PathBuf {
    resolve_gh().map_or_else(|| PathBuf::from("gh"), |(path, _)| path)
}

pub(crate) struct InstallGuard;

impl InstallGuard {
    pub(crate) fn take() -> Option<Self> {
        INSTALLING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, Ordering::SeqCst);
    }
}

fn asset_suffix() -> Result<String, String> {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macOS",
        "linux" => "linux",
        other => return Err(format!("no GitHub CLI build for {other}")),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => return Err(format!("no GitHub CLI build for {other}")),
    };
    let ext = if os == "linux" { ".tar.gz" } else { ".zip" };
    Ok(format!("_{os}_{arch}{ext}"))
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("download failed: HTTP {}", response.status()));
    }
    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("download failed: {e}"))
}

/// Release tag from the redirect of the public "latest release" page (`…/releases/tag/v2.105.0`).
async fn latest_tag() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent("Harness")
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("cannot start download: {e}"))?;
    let response = client
        .get(LATEST_PAGE)
        .send()
        .await
        .map_err(|e| format!("release lookup failed: {e}"))?;
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| format!("release lookup failed: HTTP {}", response.status()))?;
    tag_of(location).ok_or_else(|| format!("release lookup failed: unexpected redirect {location}"))
}

fn tag_of(location: &str) -> Option<String> {
    let tag = location.rsplit_once("/tag/")?.1.trim_end_matches('/');
    (tag.starts_with('v') && tag.len() > 1 && !tag.contains('/')).then(|| tag.to_owned())
}

/// (version, archive name, archive URL, checksums URL) of the latest GitHub CLI release.
async fn locate(client: &reqwest::Client) -> Result<(String, String, String, String), String> {
    let suffix = asset_suffix()?;
    // 修复：api.github.com 未登录时每个 IP 每小时只有 60 次请求，共享 IP（运营商 NAT）很快用完并返回 403，
    // 首次安装 gh 因此失败。先用公开 release 页面的跳转拿到版本号，按固定命名拼出下载地址，失败时才查 API。
    let page = match latest_tag().await {
        Ok(tag) => {
            let version = tag.trim_start_matches('v').to_owned();
            let name = format!("gh_{version}{suffix}");
            let url = format!("{DOWNLOAD_BASE}/{tag}/{name}");
            let sums = format!("{DOWNLOAD_BASE}/{tag}/gh_{version}_checksums.txt");
            return Ok((tag, name, url, sums));
        }
        Err(error) => error,
    };
    let release: serde_json::Value = match fetch(client, RELEASE_URL).await {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("unexpected release data: {e}"))?
        }
        Err(api) => {
            let hint = if api.contains("403") || api.contains("429") {
                " (GitHub API rate limit for this network; try again in an hour)"
            } else {
                ""
            };
            return Err(format!("{page}; {api}{hint}"));
        }
    };
    let version = release["tag_name"].as_str().unwrap_or_default().to_owned();
    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    let pick = |test: &dyn Fn(&str) -> bool| {
        assets.iter().find_map(|asset| {
            let name = asset["name"].as_str()?;
            let url = asset["browser_download_url"].as_str()?;
            test(name).then(|| (name.to_owned(), url.to_owned()))
        })
    };
    let (name, url) = pick(&|n: &str| n.starts_with("gh_") && n.ends_with(&suffix))
        .ok_or_else(|| format!("GitHub CLI {version} has no {suffix} build"))?;
    let (_, sums) = pick(&|n: &str| n.ends_with("_checksums.txt"))
        .ok_or_else(|| format!("GitHub CLI {version} has no checksums file"))?;
    Ok((version, name, url, sums))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    use std::fmt::Write as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

fn find_root(dir: &Path, depth: usize) -> Option<PathBuf> {
    if dir.join("bin").join(exe_name()).is_file() {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .find_map(|entry| find_root(&entry.path(), depth - 1))
}

fn unpack(tools: &Path, name: &str, bytes: Vec<u8>) -> Result<PathBuf, String> {
    let tmp = tools.join("gh.tmp");
    let target = tools.join("gh");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)
            .map_err(|e| format!("cannot clear {}: {e}", tmp.display()))?;
    }
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let result = (|| {
        let cursor = std::io::Cursor::new(bytes);
        if name.ends_with(".tar.gz") {
            tar::Archive::new(flate2::read::GzDecoder::new(cursor))
                .unpack(&tmp)
                .map_err(|e| format!("cannot extract {name}: {e}"))?;
        } else {
            zip::ZipArchive::new(cursor)
                .and_then(|mut archive| archive.extract(&tmp))
                .map_err(|e| format!("cannot extract {name}: {e}"))?;
        }
        let root = find_root(&tmp, 3).ok_or_else(|| format!("{name} has no bin/{}", exe_name()))?;
        if target.exists() {
            std::fs::remove_dir_all(&target)
                .map_err(|e| format!("cannot replace {}: {e}", target.display()))?;
        }
        std::fs::rename(&root, &target)
            .map_err(|e| format!("cannot move gh into {}: {e}", target.display()))?;
        let program = target.join("bin").join(exe_name());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("cannot make gh executable: {e}"))?;
        }
        Ok(program)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

pub(crate) async fn install(progress: impl Fn(String)) -> Result<PathBuf, String> {
    let tools = TOOLS
        .get()
        .cloned()
        .ok_or_else(|| "Harness home is not set".to_owned())?;
    let work = async {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .user_agent("Harness")
            .build()
            .map_err(|e| format!("cannot start download: {e}"))?;
        progress("Looking up the latest GitHub CLI release".to_owned());
        let (_version, name, url, sums_url) = locate(&client).await?;
        progress(format!("Downloading {name}"));
        let bytes = fetch(&client, &url).await?;
        let sums = String::from_utf8_lossy(&fetch(&client, &sums_url).await?).into_owned();
        let expected = sums
            .lines()
            .find_map(|line| {
                let mut parts = line.split_whitespace();
                let hash = parts.next()?;
                (parts.next()? == name).then(|| hash.to_ascii_lowercase())
            })
            .ok_or_else(|| format!("{name} is not in the checksums file"))?;
        if sha256_hex(&bytes) != expected {
            return Err(format!(
                "checksum mismatch for {name}; nothing was installed"
            ));
        }
        progress(format!("Checksum OK, extracting {name}"));
        tokio::fs::create_dir_all(&tools)
            .await
            .map_err(|e| format!("cannot create {}: {e}", tools.display()))?;
        let program = tokio::task::spawn_blocking(move || unpack(&tools, &name, bytes))
            .await
            .map_err(|e| format!("extract failed: {e}"))??;
        Ok(program)
    };
    tokio::time::timeout(INSTALL_LIMIT, work)
        .await
        .map_err(|_| "the GitHub CLI download did not finish within 10 minutes".to_owned())?
}

pub(crate) async fn run(
    program: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    stdin: Option<String>,
) -> Result<ToolOut, String> {
    let name = program
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut command = Command::new(program);
    command
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|e| format!("{name}: {e}"))?;
    let input = child.stdin.take();
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{name}: no stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{name}: no stderr"))?;
    let work = async {
        let write = async {
            if let (Some(mut pipe), Some(body)) = (input, stdin) {
                pipe.write_all(body.as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
                pipe.shutdown().await.map_err(|e| e.to_string())?;
            }
            Ok::<(), String>(())
        };
        let read_out = async {
            let mut out = Vec::new();
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let n = stdout.read(&mut buffer).await.map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                let room = CAP.saturating_sub(out.len());
                out.extend_from_slice(&buffer[..n.min(room)]);
            }
            Ok::<Vec<u8>, String>(out)
        };
        let mut err = Vec::new();
        let read_err = stderr.read_to_end(&mut err);
        let (written, out, err_result) = tokio::join!(write, read_out, read_err);
        written?;
        let out = out?;
        err_result.map_err(|e| e.to_string())?;
        let status = child.wait().await.map_err(|e| e.to_string())?;
        Ok::<ToolOut, String>(ToolOut {
            ok: status.success(),
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).trim().to_owned(),
        })
    };
    tokio::time::timeout(LIMIT, work).await.map_err(|_| {
        format!(
            "{name} {} did not finish in time",
            args.first().unwrap_or(&"")
        )
    })?
}

pub(crate) async fn gh(args: &[&str], stdin: Option<String>) -> Result<String, String> {
    let what = format!(
        "gh {}",
        args.iter().take(3).copied().collect::<Vec<_>>().join(" ")
    );
    run(&gh_program(), args, None, stdin)
        .await?
        .into_result(&what)
}

pub(crate) async fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let what = format!("git {}", args.first().copied().unwrap_or_default());
    let mut full = vec!["-c", "core.quotepath=false"];
    full.extend_from_slice(args);
    run(Path::new("git"), &full, Some(cwd), None)
        .await?
        .into_result(&what)
}

#[cfg(test)]
mod tests {
    use super::tag_of;

    #[test]
    fn tag_comes_from_the_latest_release_redirect() {
        assert_eq!(
            tag_of("https://github.com/cli/cli/releases/tag/v2.105.0").as_deref(),
            Some("v2.105.0")
        );
        assert_eq!(tag_of("https://github.com/cli/cli/releases"), None);
        assert_eq!(tag_of("https://github.com/cli/cli/releases/tag/"), None);
    }
}
