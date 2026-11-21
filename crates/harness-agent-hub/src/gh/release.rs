//! Finding the latest gh release and its asset URLs without the rate-limited GitHub API.

use super::{asset_suffix, fetch, DOWNLOAD_BASE, LATEST_PAGE, RELEASE_URL};

/// Release tag from the redirect of the public "latest release" page (`…/releases/tag/v2.105.0`).
pub(super) async fn latest_tag() -> Result<String, String> {
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

pub(super) fn tag_of(location: &str) -> Option<String> {
    let tag = location.rsplit_once("/tag/")?.1.trim_end_matches('/');
    (tag.starts_with('v') && tag.len() > 1 && !tag.contains('/')).then(|| tag.to_owned())
}

/// (version, archive name, archive URL, checksums URL) of the latest GitHub CLI release.
pub(super) async fn locate(
    client: &reqwest::Client,
) -> Result<(String, String, String, String), String> {
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
