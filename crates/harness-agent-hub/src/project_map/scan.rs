use super::{
    git_dir, render, BTreeMap, HashMap, Instant, Path, Value, COMMUNITIES_SHOWN, ENTRY_FILES,
    GOD_SHOWN, GRAPH_BYTES_MAX, HOT_SHOWN, SKIPPED_DIRS, WALK_DEADLINE, WALK_DEPTH, WALK_FILES_MAX,
};

#[derive(Default)]
pub(super) struct Tree {
    pub(super) dirs: BTreeMap<String, usize>,
    pub(super) subdirs: HashMap<String, BTreeMap<String, usize>>,
    pub(super) root_files: usize,
    pub(super) entries: Vec<String>,
    pub(super) partial: bool,
}

pub(super) fn walk(root: &Path) -> Tree {
    let started = Instant::now();
    let mut tree = Tree::default();
    let walker = ignore::WalkBuilder::new(root)
        .require_git(false)
        .max_depth(Some(WALK_DEPTH))
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_some_and(|t| t.is_dir())
                || !SKIPPED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
        })
        .build();
    let mut files = 0usize;
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        files += 1;
        if files > WALK_FILES_MAX || started.elapsed() > WALK_DEADLINE {
            tree.partial = true;
            break;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let parts: Vec<String> = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        match parts.as_slice() {
            [] => {}
            [_] => tree.root_files += 1,
            [top, rest @ ..] => {
                *tree.dirs.entry(top.clone()).or_default() += 1;
                if rest.len() > 1 {
                    *tree
                        .subdirs
                        .entry(top.clone())
                        .or_default()
                        .entry(rest[0].clone())
                        .or_default() += 1;
                }
            }
        }
        let name = parts.last().map(String::as_str).unwrap_or_default();
        if parts.len() <= 4 && ENTRY_FILES.contains(&name) {
            tree.entries.push(parts.join("/"));
        }
    }
    tree.entries.sort_by_key(|p| {
        let depth = p.matches('/').count();
        let name = p.rsplit('/').next().unwrap_or(p);
        let manifest = ENTRY_FILES[..7].contains(&name);
        let class = match (depth, manifest) {
            (0, _) => 0,
            (_, false) => 1,
            (_, true) => 2,
        };
        (class, depth, p.clone())
    });
    tree
}

fn hot_files(root: &Path) -> Vec<(String, usize)> {
    if git_dir(root).is_none() {
        return Vec::new();
    }
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args([
            "log",
            "-n",
            "300",
            "--no-merges",
            "--name-only",
            "--format=",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim();
        if line.is_empty() || is_noise(line) {
            continue;
        }
        *counts.entry(line.to_owned()).or_default() += 1;
    }
    let mut hot: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(path, n)| *n > 1 && root.join(path).is_file())
        .collect();
    hot.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hot.truncate(HOT_SHOWN);
    hot
}

fn is_noise(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("lock"))
        || name.ends_with("-lock.json")
        || name.ends_with("-lock.yaml")
        || name == "CHANGELOG.md"
        || path.split('/').any(|part| SKIPPED_DIRS.contains(&part))
}

struct Node {
    label: String,
    file: String,
    community: Option<i64>,
    degree: usize,
}

pub(super) struct GraphSummary {
    pub(super) god: Vec<(String, String, usize)>,
    pub(super) communities: Vec<(usize, Vec<String>, String)>,
}

fn graph_summary(root: &Path) -> Option<GraphSummary> {
    let path = crate::graphify::paths(root).graph;
    if std::fs::metadata(&path).ok()?.len() > GRAPH_BYTES_MAX {
        return None;
    }
    let graph: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    summarize(&graph)
}

pub(super) fn summarize(graph: &Value) -> Option<GraphSummary> {
    let nodes = graph.get("nodes")?.as_array()?;
    let links = graph
        .get("links")
        .or_else(|| graph.get("edges"))
        .and_then(Value::as_array);
    let id_of = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let mut degree: HashMap<String, usize> = HashMap::new();
    for link in links.into_iter().flatten() {
        for end in ["source", "target"] {
            if let Some(id) = link.get(end).and_then(id_of) {
                *degree.entry(id).or_default() += 1;
            }
        }
    }
    let mut all: Vec<Node> = nodes
        .iter()
        .filter_map(|node| {
            let id = node.get("id").and_then(id_of)?;
            let label = node
                .get("label")
                .and_then(Value::as_str)
                .map_or_else(|| id.clone(), str::to_owned);
            let file = node
                .get("source_file")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .replace('\\', "/");
            Some(Node {
                degree: degree.get(&id).copied().unwrap_or(0),
                label,
                file,
                community: node.get("community").and_then(Value::as_i64),
            })
        })
        .collect();
    all.sort_by(|a, b| b.degree.cmp(&a.degree).then_with(|| a.label.cmp(&b.label)));
    let god = all
        .iter()
        .filter(|n| n.degree > 0 && n.label != n.file)
        .take(GOD_SHOWN)
        .map(|n| (n.label.clone(), n.file.clone(), n.degree))
        .collect();
    let mut groups: BTreeMap<i64, Vec<&Node>> = BTreeMap::new();
    for node in &all {
        if let Some(community) = node.community {
            groups.entry(community).or_default().push(node);
        }
    }
    let mut communities: Vec<(usize, Vec<String>, String)> = groups
        .into_values()
        .map(|members| {
            let names = members.iter().take(3).map(|n| n.label.clone()).collect();
            let mut folders: HashMap<&str, usize> = HashMap::new();
            for member in &members {
                let folder = member.file.rsplit_once('/').map_or("", |(dir, _)| dir);
                *folders.entry(folder).or_default() += 1;
            }
            let folder = folders
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(dir, _)| dir.to_owned())
                .unwrap_or_default();
            (members.len(), names, folder)
        })
        .collect();
    communities.sort_by_key(|c| std::cmp::Reverse(c.0));
    communities.truncate(COMMUNITIES_SHOWN);
    Some(GraphSummary { god, communities })
}

pub(super) fn build(root: &Path) -> String {
    let tree = walk(root);
    let hot = hot_files(root);
    let graph = graph_summary(root);
    render(root, &tree, &hot, graph.as_ref())
}
