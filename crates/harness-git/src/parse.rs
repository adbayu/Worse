//! Parsers for git CLI output (ports of `gitCliHelpers.ts` / `gitCliRepo.ts`).

use std::collections::HashMap;

use harness_fs::locale_compare;

use crate::types::{ChangeKind, GraphCommit, GraphRef, GraphRefKind, HeadRefType, LocalBranch};
use crate::util::{js_parse_int, js_trim, normalize_git_path, normalized_lines};

mod refs;

pub use refs::{parse_branch_refs, parse_config_value, parse_graph_records, ConfigValue};

/// `GitStatusEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// Repository-relative path.
    pub path: String,
    /// Rename source.
    pub original_path: Option<String>,
    /// Kind.
    pub kind: ChangeKind,
    /// Index status.
    pub x: Option<String>,
    /// Worktree status.
    pub y: Option<String>,
    /// `?` record.
    pub is_untracked: bool,
    /// `u` record.
    pub is_conflicted: bool,
}

/// Parsed `git status --porcelain=v2 --branch -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedStatus {
    /// Branch.
    pub branch_name: Option<String>,
    /// Upstream.
    pub tracking_branch_name: Option<String>,
    /// HEAD kind.
    pub head_ref_type: HeadRefType,
    /// Ahead.
    pub ahead: u64,
    /// Behind.
    pub behind: u64,
    /// Entries in git order.
    pub entries: Vec<StatusEntry>,
}

/// `GitLineStat`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineStat {
    /// Added lines.
    pub added: u64,
    /// Removed lines.
    pub removed: u64,
    /// Set for renames.
    pub kind: Option<ChangeKind>,
    /// Rename source.
    pub original_path: Option<String>,
}

/// Insertion-ordered map with JS `Map` semantics (re-setting a key keeps its
/// position and replaces the value).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatMap {
    order: Vec<String>,
    values: HashMap<String, LineStat>,
}

impl StatMap {
    /// `map.set`.
    pub fn set(&mut self, key: String, value: LineStat) {
        if !self.values.contains_key(&key) {
            self.order.push(key.clone());
        }
        self.values.insert(key, value);
    }

    /// `map.get`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&LineStat> {
        self.values.get(key)
    }

    /// Entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &LineStat)> {
        self.order
            .iter()
            .filter_map(|key| self.values.get(key).map(|value| (key, value)))
    }
}

/// `inferKindFromStatusCode`.
fn kind_from_status_code(code: char) -> ChangeKind {
    match code {
        'A' | '?' => ChangeKind::Added,
        'D' => ChangeKind::Deleted,
        'R' | 'C' => ChangeKind::Renamed,
        _ => ChangeKind::Modified,
    }
}

/// JS `.` in a regex does not match line terminators; a `(.+)$` path group
/// therefore fails for paths containing them.
fn regex_dot_path(path: &str) -> Option<&str> {
    if path.is_empty() || path.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        None
    } else {
        Some(path)
    }
}

/// Matches `^<tag> ([^ ]{2})( [^ ]+){fields} (.+)$`, returning `(xy, path)`.
fn match_record(record: &str, fields: usize) -> Option<(Vec<char>, &str)> {
    let mut parts = record.splitn(fields + 3, ' ');
    parts.next()?;
    let xy: Vec<char> = parts.next()?.chars().collect();
    if xy.len() != 2 {
        return None;
    }
    for _ in 0..fields {
        if parts.next()?.is_empty() {
            return None;
        }
    }
    Some((xy, regex_dot_path(parts.next()?)?))
}

fn parse_ahead_behind(value: &str) -> (u64, u64) {
    let number_after = |marker: char| {
        value.char_indices().find_map(|(index, c)| {
            if c != marker {
                return None;
            }
            let digits: String = value[index + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if digits.is_empty() {
                None
            } else {
                Some(digits.parse::<u64>().unwrap_or(u64::MAX))
            }
        })
    };
    (
        number_after('+').unwrap_or(0),
        number_after('-').unwrap_or(0),
    )
}

/// `parseStatusPorcelain`.
#[must_use]
pub fn parse_status_porcelain(stdout: &str) -> ParsedStatus {
    let records: Vec<&str> = stdout.split('\0').filter(|r| !r.is_empty()).collect();
    let mut out = ParsedStatus {
        branch_name: None,
        tracking_branch_name: None,
        head_ref_type: HeadRefType::Branch,
        ahead: 0,
        behind: 0,
        entries: Vec::new(),
    };
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.starts_with("# ") {
            if let Some(head) = record.strip_prefix("# branch.head ") {
                if head == "(detached)" {
                    out.branch_name = None;
                    out.head_ref_type = HeadRefType::Detached;
                } else {
                    out.branch_name = Some(head.to_owned());
                    out.head_ref_type = HeadRefType::Branch;
                }
            } else if let Some(upstream) = record.strip_prefix("# branch.upstream ") {
                out.tracking_branch_name = Some(upstream.to_owned());
            } else if let Some(ab) = record.strip_prefix("# branch.ab ") {
                (out.ahead, out.behind) = parse_ahead_behind(ab);
            }
            continue;
        }
        if let Some(path) = record.strip_prefix("? ") {
            out.entries.push(StatusEntry {
                path: normalize_git_path(path),
                original_path: None,
                kind: ChangeKind::Added,
                x: None,
                y: Some("?".to_owned()),
                is_untracked: true,
                is_conflicted: false,
            });
            continue;
        }
        if record.starts_with("1 ") {
            let Some((xy, path)) = match_record(record, 6) else {
                continue;
            };
            let code = if xy[0] == '.' { xy[1] } else { xy[0] };
            out.entries.push(StatusEntry {
                path: normalize_git_path(path),
                original_path: None,
                kind: kind_from_status_code(code),
                x: Some(xy[0].to_string()),
                y: Some(xy[1].to_string()),
                is_untracked: false,
                is_conflicted: false,
            });
            continue;
        }
        if record.starts_with("2 ") {
            let Some((xy, path)) = match_record(record, 7) else {
                continue;
            };
            let original = records.get(index).copied();
            index += 1;
            out.entries.push(StatusEntry {
                path: normalize_git_path(path),
                original_path: original.filter(|o| !o.is_empty()).map(normalize_git_path),
                kind: ChangeKind::Renamed,
                x: Some(xy[0].to_string()),
                y: Some(xy[1].to_string()),
                is_untracked: false,
                is_conflicted: false,
            });
            continue;
        }
        if record.starts_with("u ") {
            let Some((xy, path)) = match_record(record, 8) else {
                continue;
            };
            out.entries.push(StatusEntry {
                path: normalize_git_path(path),
                original_path: None,
                kind: ChangeKind::Modified,
                x: Some(xy[0].to_string()),
                y: Some(xy[1].to_string()),
                is_untracked: false,
                is_conflicted: true,
            });
        }
    }
    out
}

fn numstat_value(value: &str) -> u64 {
    if value == "-" {
        return 0;
    }
    js_parse_int(value).map_or(0, |n| u64::try_from(n).unwrap_or(0))
}

/// `parseNumstat` (`git diff --numstat -z`).
#[must_use]
pub fn parse_numstat(stdout: &str) -> StatMap {
    let records: Vec<&str> = stdout.split('\0').collect();
    let mut stats = StatMap::default();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.is_empty() {
            continue;
        }
        let fields: Vec<&str> = record.split('\t').collect();
        if fields.len() < 3 {
            continue;
        }
        let added = numstat_value(fields[0]);
        let removed = numstat_value(fields[1]);
        let path_field = fields[2..].join("\t");
        if !path_field.is_empty() {
            stats.set(
                normalize_git_path(&path_field),
                LineStat {
                    added,
                    removed,
                    ..LineStat::default()
                },
            );
            continue;
        }
        let original = records.get(index).copied().unwrap_or_default();
        let renamed = records.get(index + 1).copied().unwrap_or_default();
        index += 2;
        if renamed.is_empty() {
            continue;
        }
        stats.set(
            normalize_git_path(renamed),
            LineStat {
                added,
                removed,
                kind: Some(ChangeKind::Renamed),
                original_path: Some(normalize_git_path(original)),
            },
        );
    }
    stats
}

/// `inferKindFromNumstat`.
#[must_use]
pub fn kind_from_numstat(stat: &LineStat) -> ChangeKind {
    if let Some(kind) = stat.kind {
        return kind;
    }
    if stat.added > 0 && stat.removed == 0 {
        return ChangeKind::Added;
    }
    if stat.removed > 0 && stat.added == 0 {
        return ChangeKind::Deleted;
    }
    ChangeKind::Modified
}

fn seconds_to_ms(value: Option<&str>) -> Option<i64> {
    value
        .filter(|v| !v.is_empty())
        .and_then(js_parse_int)
        .map(|s| s.saturating_mul(1000))
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.is_empty()).map(str::to_owned)
}
