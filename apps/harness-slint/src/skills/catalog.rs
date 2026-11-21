use super::{
    push, skill_catalog, CatalogSkill, Duration, HashMap, Instant, PathBuf, PlanOp, Req, TreeEntry,
    UnboundedSender, INDEX_URL, RAW_ROOT, STATE, TREE_URL,
};

fn cache_file(cache: &std::path::Path, name: &str) -> PathBuf {
    cache.join(name)
}

pub(super) fn load_cache() {
    STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(s) = cell.as_mut() else {
            return;
        };
        let cache = s.cache.clone();
        if let Ok(bytes) = std::fs::read(cache_file(&cache, "index.json")) {
            if let Ok(catalog) = skill_catalog::parse_catalog(&bytes) {
                s.catalog = catalog.skills;
                s.available = true;
            }
        }
        if let Ok(bytes) = std::fs::read(cache_file(&cache, "tree.json")) {
            if let Ok(tree) = skill_catalog::parse_tree(&bytes) {
                s.tree = tree;
            }
        }
        if let Ok(text) = std::fs::read_to_string(cache_file(&cache, "installed.json")) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, Vec<(String, String)>>>(&text) {
                s.meta = map;
            }
        }
    });
}

pub(super) fn save_meta() {
    let (cache, meta) = STATE.with(|cell| {
        let cell = cell.borrow();
        let Some(s) = cell.as_ref() else {
            return (PathBuf::new(), HashMap::new());
        };
        (s.cache.clone(), s.meta.clone())
    });
    if cache.as_os_str().is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(&cache);
    if let Ok(text) = serde_json::to_string(&meta) {
        let _ = std::fs::write(cache_file(&cache, "installed.json"), text);
    }
}

fn client() -> Option<reqwest::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()
}

pub(super) fn spawn_revalidate(cache: PathBuf) {
    STATE.with(|cell| {
        if let Some(s) = cell.borrow_mut().as_mut() {
            s.fetching = true;
        }
    });
    push();
    std::thread::spawn(move || {
        let result = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(fetch_catalog(cache)),
            Err(error) => Err(format!("runtime: {error}")),
        };
        let _ = slint::invoke_from_event_loop(move || {
            let failed = result.is_err();
            load_cache();
            STATE.with(|cell| {
                let mut cell = cell.borrow_mut();
                let Some(s) = cell.as_mut() else {
                    return;
                };
                s.fetching = false;
                s.last = Some(Instant::now());
                if failed {
                    s.available = !s.catalog.is_empty();
                    s.status = if s.available {
                        String::new()
                    } else {
                        "The skill catalog cannot be loaded".to_string()
                    };
                    s.status_error = !s.available;
                }
            });
            push();
        });
    });
}

async fn fetch_catalog(cache: PathBuf) -> Result<(), String> {
    let client = client().ok_or_else(|| "cannot build the HTTP client".to_string())?;
    let etag_path = cache_file(&cache, "index.etag");
    let etag = std::fs::read_to_string(&etag_path).ok();
    let mut request = client.get(INDEX_URL);
    if let Some(tag) = etag.as_deref() {
        request = request.header(reqwest::header::IF_NONE_MATCH, tag);
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    if response.status() != reqwest::StatusCode::NOT_MODIFIED {
        if !response.status().is_success() {
            return Err(format!("catalog index: HTTP {}", response.status()));
        }
        let tag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = response.bytes().await.map_err(|e| e.to_string())?;
        skill_catalog::parse_catalog(&bytes).map_err(|e| e.to_string())?;
        let _ = std::fs::create_dir_all(&cache);
        let _ = std::fs::write(cache_file(&cache, "index.json"), &bytes);
        if let Some(tag) = tag {
            let _ = std::fs::write(&etag_path, tag);
        }
    }
    let tree = client
        .get(TREE_URL)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !tree.status().is_success() {
        return Err(format!("file tree: HTTP {}", tree.status()));
    }
    let bytes = tree.bytes().await.map_err(|e| e.to_string())?;
    skill_catalog::parse_tree(&bytes).map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(&cache);
    let _ = std::fs::write(cache_file(&cache, "tree.json"), &bytes);
    Ok(())
}

pub(super) fn spawn_download(req: UnboundedSender<Req>, entry: CatalogSkill) {
    let (tree, group) = STATE.with(|cell| {
        let cell = cell.borrow();
        let Some(s) = cell.as_ref() else {
            return (Vec::new(), entry.group.clone());
        };
        (s.tree.clone(), entry.group.clone())
    });
    std::thread::spawn(move || {
        let name = entry.name.clone();
        let result = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(download(&entry, &tree, &group)),
            Err(error) => Err(format!("runtime: {error}")),
        };
        match result {
            Ok((skill, files, shas)) => {
                let _ = slint::invoke_from_event_loop(move || {
                    STATE.with(|cell| {
                        if let Some(s) = cell.borrow_mut().as_mut() {
                            s.last_install = Some((name, shas));
                        }
                    });
                });
                let _ = req.send(Req::Plan {
                    title: "install skill",
                    op: PlanOp::InstallCatalog {
                        name: entry.name,
                        skill: Box::new(skill),
                        extra: files,
                    },
                });
            }
            Err(error) => {
                let _ = slint::invoke_from_event_loop(move || {
                    STATE.with(|cell| {
                        let mut cell = cell.borrow_mut();
                        if let Some(s) = cell.as_mut() {
                            s.busy = String::new();
                            s.status = error;
                            s.status_error = true;
                        }
                    });
                    push();
                });
            }
        }
    });
}

async fn download(
    entry: &CatalogSkill,
    tree: &[TreeEntry],
    group: &str,
) -> Result<
    (
        harness_agent_config::Skill,
        Vec<(String, String)>,
        Vec<(String, String)>,
    ),
    String,
> {
    let client = client().ok_or_else(|| "cannot build the HTTP client".to_string())?;
    let text = get_text(&client, &entry.entrypoint).await?;
    let skill = skill_catalog::parse_skill_md(&entry.name, &text);
    let listed = skill_catalog::skill_files(tree, group, &entry.name);
    let mut files = Vec::new();
    for (rel, _sha) in &listed {
        let encoded = rel
            .split('/')
            .map(percent_encode)
            .collect::<Vec<_>>()
            .join("/");
        let url = format!("{RAW_ROOT}skills/{group}/{}/{encoded}", entry.name);
        if let Ok(text) = get_text(&client, &url).await {
            files.push((rel.clone(), text));
        }
    }
    Ok((skill, files, listed))
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {} for {url}", response.status()));
    }
    response.text().await.map_err(|e| e.to_string())
}

fn percent_encode(segment: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::new();
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(byte >> 4)] as char);
            out.push(HEX[usize::from(byte & 0x0f)] as char);
        }
    }
    out
}
