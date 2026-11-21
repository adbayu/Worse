use super::{
    js_trim, locale_compare, non_empty, normalized_lines, seconds_to_ms, GraphCommit, GraphRef,
    GraphRefKind, LocalBranch,
};

/// `parseBranchRefRecords` (including its sort).
#[must_use]
pub fn parse_branch_refs(stdout: &str, current: Option<&str>) -> Vec<LocalBranch> {
    let mut branches: Vec<LocalBranch> = normalized_lines(stdout)
        .into_iter()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let name = fields.next().filter(|n| !n.is_empty())?;
            let upstream = fields.next();
            let hash = fields.next();
            let timestamp = fields.next();
            Some(LocalBranch {
                name: name.to_owned(),
                is_current: Some(name) == current,
                upstream_name: non_empty(upstream),
                commit_hash: non_empty(hash),
                commit_timestamp_ms: seconds_to_ms(timestamp),
            })
        })
        .collect();
    branches.sort_by(|left, right| {
        right
            .is_current
            .cmp(&left.is_current)
            .then_with(|| {
                let l = left.commit_timestamp_ms.unwrap_or(i64::MIN);
                let r = right.commit_timestamp_ms.unwrap_or(i64::MIN);
                r.cmp(&l)
            })
            .then_with(|| locale_compare(&left.name, &right.name))
    });
    branches
}

pub(super) fn add_graph_ref(refs: &mut Vec<GraphRef>, candidate: GraphRef) {
    if !refs
        .iter()
        .any(|r| r.kind == candidate.kind && r.name == candidate.name)
    {
        refs.push(candidate);
    }
}

pub(super) fn named(name: &str, kind: GraphRefKind) -> Option<GraphRef> {
    if name.is_empty() {
        None
    } else {
        Some(GraphRef {
            name: name.to_owned(),
            kind,
        })
    }
}

/// `parseGitGraphDecorationRef`.
pub(super) fn parse_decoration_ref(raw: &str) -> Option<GraphRef> {
    let r = js_trim(raw);
    if r.is_empty() {
        return None;
    }
    if r == "HEAD" {
        return named("HEAD", GraphRefKind::Head);
    }
    if let Some(tag) = r.strip_prefix("tag: ") {
        let tag = js_trim(tag);
        return named(
            tag.strip_prefix("refs/tags/").unwrap_or(tag),
            GraphRefKind::Tag,
        );
    }
    if let Some(name) = r.strip_prefix("refs/heads/") {
        return named(name, GraphRefKind::Branch);
    }
    if let Some(name) = r.strip_prefix("refs/remotes/") {
        return named(name, GraphRefKind::Remote);
    }
    if let Some(name) = r.strip_prefix("refs/tags/") {
        return named(name, GraphRefKind::Tag);
    }
    let kind = if r.contains('/') {
        GraphRefKind::Remote
    } else {
        GraphRefKind::Branch
    };
    named(r, kind)
}

/// `parseGitGraphRefs`.
pub(super) fn parse_graph_refs(raw: &str) -> Vec<GraphRef> {
    let mut refs = Vec::new();
    for decoration in raw.split(',') {
        let decoration = js_trim(decoration);
        if decoration.is_empty() {
            continue;
        }
        if let Some(pointed) = decoration.strip_prefix("HEAD -> ") {
            add_graph_ref(
                &mut refs,
                GraphRef {
                    name: "HEAD".to_owned(),
                    kind: GraphRefKind::Head,
                },
            );
            if let Some(r) = parse_decoration_ref(pointed) {
                add_graph_ref(&mut refs, r);
            }
            continue;
        }
        if let Some(r) = parse_decoration_ref(decoration) {
            add_graph_ref(&mut refs, r);
        }
    }
    refs
}

/// `parseGitGraphRecords` (`%H%x00%P%x00%an%x00%at%x00%s%x00%D%x1e`).
#[must_use]
pub fn parse_graph_records(stdout: &str) -> Vec<GraphCommit> {
    stdout
        .split('\u{1e}')
        .map(js_trim)
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let mut fields = record.split('\0');
            let hash = fields.next().filter(|h| !h.is_empty())?;
            let parents = fields.next();
            let author = fields.next();
            let authored = fields.next();
            let subject = fields.next();
            let decorations = fields.next();
            Some(GraphCommit {
                hash: hash.to_owned(),
                parents: parents
                    .map(|p| {
                        p.split(' ')
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                refs: parse_graph_refs(decorations.unwrap_or_default()),
                subject: subject.unwrap_or_default().to_owned(),
                author_name: non_empty(author),
                authored_at_ms: seconds_to_ms(authored),
            })
        })
        .collect()
}

/// `parseGitConfigValue` result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigValue {
    /// `--show-scope` column.
    pub scope: Option<String>,
    /// `--show-origin` column.
    pub source: Option<String>,
    /// Value.
    pub value: Option<String>,
}

/// `parseGitConfigValue` for an already-successful (exit 0) result.
#[must_use]
pub fn parse_config_value(stdout: &str) -> ConfigValue {
    let line = stdout
        .strip_suffix('\n')
        .map_or(stdout, |l| l.strip_suffix('\r').unwrap_or(l));
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() < 3 {
        return ConfigValue {
            value: non_empty(Some(line)),
            ..ConfigValue::default()
        };
    }
    ConfigValue {
        scope: Some(parts[0].to_owned()),
        source: Some(parts[1].to_owned()),
        value: non_empty(Some(&parts[2..].join("\t"))),
    }
}
