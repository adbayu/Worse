//! Folds a run of consecutive tool and thinking rows into one summary line ("Ran 3 commands, read 2 files"),
//! like the web chat clients. The lead row of a run carries the summary; the other rows hide until it opens.

use slint::{Model, ModelRc, SharedString, VecModel};

use super::UiState;
use crate::ui::{ChatImage, ChatItem};

fn groupable(row: &ChatItem) -> bool {
    row.kind == 2 || row.kind == 5
}

fn bucket(kind: &str) -> &'static str {
    match kind {
        "execute" => "execute",
        "read" => "read",
        "edit" | "move" => "edit",
        "search" => "search",
        "fetch" => "fetch",
        "delete" => "delete",
        _ => "other",
    }
}

fn phrase(kind: &str, count: usize) -> String {
    let (verb, one, many) = match kind {
        "execute" => ("ran", "command", "commands"),
        "read" => ("read", "file", "files"),
        "edit" => ("edited", "file", "files"),
        "search" => ("ran", "search", "searches"),
        "fetch" => ("fetched", "page", "pages"),
        "delete" => ("deleted", "file", "files"),
        _ => ("used", "tool", "tools"),
    };
    if count == 1 {
        format!("{verb} a {one}")
    } else {
        format!("{verb} {count} {many}")
    }
}

fn summary(tools: &[&ChatItem]) -> String {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for tool in tools {
        let key = bucket(tool.tool_kind.as_str());
        match counts.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 += 1,
            None => counts.push((key, 1)),
        }
    }
    let text = counts
        .iter()
        .map(|(kind, count)| phrase(kind, *count))
        .collect::<Vec<_>>()
        .join(", ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

impl UiState {
    pub(super) fn mark_continued(&self, index: usize) {
        let Some(mut row) = self.chat.row_data(index) else {
            return;
        };
        let mut previous = None;
        let mut agent_before = false;
        for at in (0..index).rev() {
            match self.chat.row_data(at).map(|r| r.kind) {
                Some(1) => {
                    agent_before = true;
                    previous = Some(at);
                    break;
                }
                Some(2 | 5) => agent_before = true,
                Some(0 | 6) | None => break,
                _ => {}
            }
        }
        if row.kind == 2 || row.kind == 5 {
            let head = !agent_before;
            if row.head != head {
                row.head = head;
                self.chat.set_row_data(index, row);
            }
            return;
        }
        if row.kind != 1 {
            return;
        }
        let mut end = true;
        for at in index + 1..self.chat.row_count() {
            match self.chat.row_data(at).map(|r| r.kind) {
                Some(1) => {
                    end = false;
                    break;
                }
                Some(0 | 6) | None => break,
                _ => {}
            }
        }
        let continued = agent_before;
        if row.continued != continued || row.turn_end != end {
            row.continued = continued;
            row.turn_end = end;
            self.chat.set_row_data(index, row);
        }
        if let Some(at) = previous {
            if let Some(mut before) = self.chat.row_data(at).filter(|r| r.turn_end) {
                before.turn_end = false;
                self.chat.set_row_data(at, before);
            }
        }
    }

    pub(super) fn regroup(&self, index: usize) {
        let count = self.chat.row_count();
        if !self.chat.row_data(index).is_some_and(|r| groupable(&r)) {
            return;
        }
        let mut start = index;
        while start > 0 && self.chat.row_data(start - 1).is_some_and(|r| groupable(&r)) {
            start -= 1;
        }
        let mut end = index + 1;
        while end < count && self.chat.row_data(end).is_some_and(|r| groupable(&r)) {
            end += 1;
        }
        let rows: Vec<ChatItem> = (start..end).filter_map(|i| self.chat.row_data(i)).collect();
        let tools: Vec<&ChatItem> = rows.iter().filter(|r| r.kind == 2).collect();
        let waiting = tools.iter().any(|r| r.status == 1);
        let grouped = tools.len() >= 2 && !waiting;
        let open = rows.first().is_some_and(|r| r.group_open);
        let label = SharedString::from(if grouped {
            summary(&tools)
        } else {
            String::new()
        });
        let state = if tools.iter().any(|r| r.status == 0) {
            0
        } else if tools.iter().any(|r| r.status == 4) {
            4
        } else {
            2
        };
        let size = i32::try_from(tools.len()).unwrap_or(i32::MAX);
        let added: i32 = tools.iter().map(|r| r.added).sum();
        let removed: i32 = tools.iter().map(|r| r.removed).sum();
        let mut thumbs: Vec<ChatImage> = Vec::new();
        let mut extra = 0;
        for tool in &tools {
            for image in tool.images.iter() {
                if thumbs.len() < 3 {
                    thumbs.push(image);
                } else {
                    extra += 1;
                }
            }
            extra += tool.images_more;
        }
        let thumb_count = thumbs.len();
        let updates: Vec<(usize, ChatItem)> = rows
            .into_iter()
            .enumerate()
            .filter_map(|(offset, mut row)| {
                let lead = offset == 0;
                let group = if grouped && lead { size } else { 0 };
                let folded = grouped && !lead && !open;
                let row_label = if group > 0 {
                    label.clone()
                } else {
                    SharedString::new()
                };
                let row_state = if group > 0 { state } else { 0 };
                let member = grouped;
                let (row_added, row_removed, row_more) = if group > 0 {
                    (added, removed, extra)
                } else {
                    (0, 0, 0)
                };
                let row_thumbs = if group > 0 { thumb_count } else { 0 };
                if row.group == group
                    && row.folded == folded
                    && row.group_label == row_label
                    && row.group_state == row_state
                    && row.member == member
                    && row.group_added == row_added
                    && row.group_removed == row_removed
                    && row.group_images_more == row_more
                    && row.group_images.row_count() == row_thumbs
                {
                    return None;
                }
                row.group = group;
                row.folded = folded;
                row.group_label = row_label;
                row.group_state = row_state;
                row.member = member;
                row.group_added = row_added;
                row.group_removed = row_removed;
                row.group_images_more = row_more;
                row.group_images = if group > 0 {
                    ModelRc::new(VecModel::from(thumbs.clone()))
                } else {
                    ModelRc::default()
                };
                Some((start + offset, row))
            })
            .collect();
        for (at, row) in updates {
            self.chat.set_row_data(at, row);
        }
    }

    /// Marks the head row (agent name) of the run holding `index` as streaming while a later answer in the
    /// same run still streams, so "Streaming…" stays next to the agent name instead of on its own line.
    pub(super) fn sync_streaming(&self, index: usize) {
        let boundary = |at: usize| {
            self.chat
                .row_data(at)
                .map_or(true, |r| matches!(r.kind, 0 | 6 | 7))
        };
        if boundary(index) {
            return;
        }
        let mut start = index;
        while start > 0 && !boundary(start - 1) {
            start -= 1;
        }
        let mut end = index + 1;
        while end < self.chat.row_count() && !boundary(end) {
            end += 1;
        }
        let Some(head) = (start..end).find(|&at| {
            self.chat
                .row_data(at)
                .is_some_and(|r| matches!(r.kind, 1 | 2 | 5))
        }) else {
            return;
        };
        let live = (head + 1..end).any(|at| {
            self.chat
                .row_data(at)
                .is_some_and(|r| r.kind == 1 && r.status == 0)
        });
        if let Some(mut row) = self.chat.row_data(head).filter(|r| r.streaming != live) {
            row.streaming = live;
            self.chat.set_row_data(head, row);
        }
    }

    pub(super) fn toggle_group(&self, index: usize) {
        if let Some(mut row) = self.chat.row_data(index) {
            row.group_open = !row.group_open;
            self.chat.set_row_data(index, row);
            self.regroup(index);
        }
    }
}
