use super::{ScopeView, ToolCallView};

/// A subagent's permissions: workspace paths, tool title fragments, network and a deadline. Empty paths or
/// tools mean "no restriction", so a scope only ever narrows the parent's (§9.8.2).
pub(crate) fn narrow(
    parent: Option<&ScopeView>,
    request: &ScopeView,
    now: i64,
) -> Result<ScopeView, String> {
    let parent_paths = parent.map(|p| &p.paths).filter(|p| !p.is_empty());
    let paths = match parent_paths {
        None => request.paths.clone(),
        Some(allowed) => {
            let wider = |path: &str| !allowed.iter().any(|a| within(a, path));
            if request.paths.is_empty() {
                return Err(format!(
                    "paths are wider than the parent scope, which allows only: {}",
                    allowed.join(", ")
                ));
            }
            if let Some(path) = request.paths.iter().find(|p| wider(p.as_str())) {
                return Err(format!(
                    "{path} is wider than the parent scope, which allows only: {}",
                    allowed.join(", ")
                ));
            }
            request.paths.clone()
        }
    };
    let parent_tools = parent.map(|p| &p.tools).filter(|t| !t.is_empty());
    let tools = match parent_tools {
        None => request.tools.clone(),
        Some(allowed) => {
            if request.tools.is_empty() {
                return Err(format!(
                    "tools are wider than the parent scope, which allows only: {}",
                    allowed.join(", ")
                ));
            }
            let wider = request
                .tools
                .iter()
                .find(|t| !allowed.iter().any(|a| a.eq_ignore_ascii_case(t.as_str())));
            if let Some(tool) = wider {
                return Err(format!(
                    "{tool} is wider than the parent scope, which allows only: {}",
                    allowed.join(", ")
                ));
            }
            request.tools.clone()
        }
    };
    if parent.is_some_and(|p| !p.network) && request.network {
        return Err("network is off in the parent scope".to_owned());
    }
    let network = request.network;
    let expires_at = match (parent.and_then(|p| p.expires_at), request.expires_at) {
        (Some(limit), Some(want)) if want > limit => {
            let mins = (limit - now).max(0) / 60_000;
            return Err(format!(
                "the deadline is past the parent scope, which ends in {mins} min"
            ));
        }
        (Some(limit), None) => Some(limit),
        (_, want) => want,
    };
    Ok(ScopeView {
        paths,
        tools,
        network,
        expires_at,
    })
}

/// Whether `path` (already workspace-relative) lies inside `allowed`.
fn within(allowed: &str, path: &str) -> bool {
    let allowed = allowed.to_lowercase();
    let path = path.to_lowercase();
    path == allowed || path.starts_with(&format!("{allowed}/"))
}

/// Whether the scope lets the subagent touch `path`.
pub(crate) fn allows_path(scope: &ScopeView, path: &str) -> bool {
    scope.paths.is_empty() || scope.paths.iter().any(|allowed| within(allowed, path))
}

/// Why `scope` blocks the tool call, naming the rule that blocks it (`None` = allowed).
pub(crate) fn blocked(
    scope: &ScopeView,
    tool: &ToolCallView,
    paths: &[String],
    now: i64,
) -> Option<String> {
    if scope.expires_at.is_some_and(|deadline| now >= deadline) {
        return Some("its scope deadline has passed".to_owned());
    }
    if !scope.tools.is_empty() {
        let title = tool.title.to_lowercase();
        if !scope
            .tools
            .iter()
            .any(|t| title.contains(&t.to_lowercase()))
        {
            return Some(format!(
                "its scope allows only these tools: {}",
                scope.tools.join(", ")
            ));
        }
    }
    if !scope.network && uses_network(tool) {
        return Some("network access is off in its scope".to_owned());
    }
    if let Some(path) = paths.iter().find(|path| !allows_path(scope, path.as_str())) {
        return Some(format!(
            "{path} is outside its scope, which allows only: {}",
            scope.paths.join(", ")
        ));
    }
    None
}

/// Whether the tool call looks like it reaches the network.
fn uses_network(tool: &ToolCallView) -> bool {
    const WORDS: [&str; 8] = [
        "http", "web", "fetch", "curl", "wget", "download", "network", "browser",
    ];
    let haystack = format!(
        "{} {}",
        tool.title,
        tool.raw_input.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    WORDS.iter().any(|word| haystack.contains(word))
}

/// One line describing the scope for the task preamble and the subagent card.
pub(crate) fn describe(scope: &ScopeView, now: i64) -> String {
    let mut parts = vec![if scope.paths.is_empty() {
        "whole workspace".to_owned()
    } else {
        scope.paths.join(", ")
    }];
    if !scope.tools.is_empty() {
        parts.push(scope.tools.join(", "));
    }
    if !scope.network {
        parts.push("no network".to_owned());
    }
    if let Some(deadline) = scope.expires_at {
        let mins = (deadline - now).max(0) / 60_000;
        parts.push(if mins >= 1 {
            format!("{mins} min left")
        } else {
            "ending now".to_owned()
        });
    }
    parts.join(" · ")
}

/// `path` relative to `workspace` (unchanged when it lies elsewhere).
pub(crate) fn relative_to(workspace: &str, path: &str) -> String {
    let path = path.replace('\\', "/");
    let root = workspace.replace('\\', "/");
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return path;
    }
    let lower_path = path.to_lowercase();
    let lower_root = root.to_lowercase();
    if lower_path == lower_root {
        return String::new();
    }
    match lower_path.strip_prefix(&format!("{lower_root}/")) {
        Some(rest) => path[path.len() - rest.len()..].to_owned(),
        None => path,
    }
}
