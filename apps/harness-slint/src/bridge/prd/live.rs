//! PRD page ↔ Orchestra live data (PRD mode plan, milestone 4): which subagent works on which task. In PRD
//! mode the agent starts each delegated task with "<TASK id>: <title>", and that first line becomes the
//! subagent title, so a card finds its subagent by the TASK id in the title.

/// One subagent of the open session, from `ChatUi.fleet`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Sub {
    pub(super) agent: String,
    pub(super) title: String,
    /// `SubagentRow.status`: 0 running, 1 needs approval, 2 done, 3 failed, 4 cancelled, 5 interrupted.
    pub(super) status: i32,
    pub(super) activity: String,
    pub(super) started: i32,
}

/// `key` appears in `title` as a whole id (case-insensitive): "TASK-1" does not match "TASK-12".
fn mentions(title: &str, key: &str) -> bool {
    let title = title.to_ascii_lowercase();
    let key = key.trim().to_ascii_lowercase();
    if key.is_empty() {
        return false;
    }
    let id_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    title.match_indices(&key).any(|(at, _)| {
        let before = title[..at].chars().next_back();
        let after = title[at + key.len()..].chars().next();
        !before.is_some_and(id_char) && !after.is_some_and(id_char)
    })
}

/// The subagent of a task: an active one first (running, needs approval), else the latest started.
pub(super) fn find<'a>(subs: &'a [Sub], key: &str) -> Option<&'a Sub> {
    subs.iter()
        .filter(|s| mentions(&s.title, key))
        .max_by_key(|s| (s.status <= 1, s.started))
}

/// Upper-case first letter of an agent name for the card avatar.
pub(super) fn initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{find, initial, mentions, Sub};

    fn sub(title: &str, status: i32, started: i32) -> Sub {
        Sub {
            agent: "Codex".into(),
            title: title.into(),
            status,
            started,
            ..Sub::default()
        }
    }

    #[test]
    fn ids_match_whole_tokens_only() {
        assert!(mentions("TASK-1: login form", "task-1"));
        assert!(mentions("Run (TASK-1)", "TASK-1"));
        assert!(!mentions("TASK-12: logout", "TASK-1"));
        assert!(!mentions("SUBTASK-1", "TASK-1"));
        assert!(!mentions("anything", " "));
    }

    #[test]
    fn active_subagent_wins_over_newer_finished_one() {
        let subs = [sub("TASK-1: a", 0, 10), sub("TASK-1: a again", 2, 20)];
        assert_eq!(find(&subs, "TASK-1").map(|s| s.status), Some(0));
        let subs = [sub("TASK-1: a", 3, 10), sub("TASK-1: a again", 2, 20)];
        assert_eq!(find(&subs, "TASK-1").map(|s| s.started), Some(20));
        assert_eq!(find(&subs, "TASK-2"), None);
        assert_eq!(initial(" deepSeek"), "D");
        assert_eq!(initial(""), "");
    }
}
