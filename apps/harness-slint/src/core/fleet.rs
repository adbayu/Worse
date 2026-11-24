use std::collections::HashMap;

use harness_core_api::{self as api, EventKind, HubEvent};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FleetSession {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub agent: String,
    pub running: bool,
    pub open: bool,
    /// Every subagent under the session, depth first: direct ones and the ones they delegated in turn.
    pub subagents: Vec<api::SubagentView>,
    /// Per entry of `subagents`: index of its parent in that list (`None` = the session itself).
    pub parents: Vec<Option<usize>>,
}

#[derive(Default)]
struct Entry {
    agent: String,
    running: bool,
    prompt: String,
    queued: HashMap<api::TurnId, String>,
    subagents: Vec<api::SubagentView>,
}

#[derive(Default)]
pub(super) struct Fleet {
    entries: HashMap<String, Entry>,
    titles: HashMap<String, String>,
    children: HashMap<String, String>,
    order: Vec<String>,
    open: Option<String>,
    shown: Option<Vec<FleetSession>>,
    tool_meta: HashMap<String, (Option<String>, Vec<api::ToolLocation>)>,
}

impl Fleet {
    pub(super) fn set_sessions(&mut self, list: &[api::SessionSummary]) {
        self.titles.clear();
        self.children.clear();
        self.order.clear();
        for s in list {
            self.titles.insert(s.id.0.clone(), s.title.clone());
            self.order.push(s.id.0.clone());
            if let Some(parent) = &s.parent_id {
                self.children.insert(s.id.0.clone(), parent.0.clone());
            }
            let entry = self.entries.entry(s.id.0.clone()).or_default();
            if entry.agent.is_empty() || !entry.running {
                entry.agent.clone_from(&s.active_agent.0);
            }
        }
        let titles = &self.titles;
        self.entries.retain(|id, _| titles.contains_key(id));
    }

    pub(super) fn set_open(&mut self, id: &str, subagents: &[api::SubagentView], prompt: &str) {
        self.open = Some(id.to_owned());
        let entry = self.entries.entry(id.to_owned()).or_default();
        entry.subagents = subagents.to_vec();
        if !prompt.is_empty() {
            prompt.clone_into(&mut entry.prompt);
        }
    }

    pub(super) fn set_running(&mut self, id: &str, running: bool) {
        self.entries.entry(id.to_owned()).or_default().running = running;
    }

    pub(super) fn on_event(&mut self, event: &HubEvent) -> bool {
        let Some(session) = event.session_id.as_ref().map(|s| s.0.clone()) else {
            return false;
        };
        match &event.kind {
            EventKind::TurnQueued {
                turn_id, user_text, ..
            } => {
                let entry = self.entries.entry(session).or_default();
                entry.queued.insert(turn_id.clone(), user_text.clone());
                if entry.prompt.is_empty() {
                    entry.prompt.clone_from(user_text);
                }
                true
            }
            EventKind::TurnStarted { turn_id, agent, .. } => {
                let entry = self.entries.entry(session).or_default();
                entry.running = true;
                entry.agent.clone_from(&agent.0);
                if let Some(text) = entry.queued.remove(turn_id) {
                    entry.prompt = text;
                }
                true
            }
            EventKind::TurnFinished { .. } | EventKind::TurnPaused { .. } => {
                self.entries.entry(session).or_default().running = false;
                true
            }
            EventKind::AgentSwitched { to, .. } => {
                self.entries
                    .entry(session)
                    .or_default()
                    .agent
                    .clone_from(&to.0);
                true
            }
            EventKind::SubagentUpdated { subagent } => {
                let parent = subagent.parent_id.0.clone();
                self.children
                    .insert(subagent.session_id.0.clone(), parent.clone());
                let list = &mut self.entries.entry(parent).or_default().subagents;
                match list
                    .iter_mut()
                    .find(|s| s.session_id == subagent.session_id)
                {
                    Some(slot) => *slot = subagent.clone(),
                    None => list.insert(0, subagent.clone()),
                }
                true
            }
            _ => false,
        }
    }

    /// A file tool call of a subagent under the open session `open`, as an event for the Live tab. Tool updates
    /// often omit the kind or locations, so the last ones seen per tool call fill them in.
    pub(super) fn subagent_tool(&mut self, open: &str, event: &HubEvent) -> Option<super::Event> {
        let session = event.session_id.as_ref()?.0.as_str();
        let (EventKind::ToolCall { tool, .. } | EventKind::ToolCallUpdated { tool, .. }) =
            &event.kind
        else {
            return None;
        };
        let root = self.root_of(session)?;
        if root != open {
            return None;
        }
        if self.tool_meta.len() > 512 {
            self.tool_meta.clear();
        }
        let meta = self.tool_meta.entry(tool.tool_call_id.clone()).or_default();
        if tool.kind.is_some() {
            meta.0.clone_from(&tool.kind);
        }
        if !tool.locations.is_empty() {
            meta.1.clone_from(&tool.locations);
        }
        let (kind, locations) = meta.clone();
        let has_diff = tool
            .content
            .iter()
            .any(|c| matches!(c, api::ToolContent::Diff { .. }));
        if locations.is_empty() && !has_diff {
            return None;
        }
        Some(super::Event::SubagentTool {
            root,
            title: self.subagent_title(session),
            kind,
            content: tool.content.clone(),
            locations,
        })
    }

    /// Top-level session `id` belongs to, when `id` is a subagent.
    pub(super) fn root_of(&self, id: &str) -> Option<String> {
        let mut current = self.children.get(id)?;
        for _ in 0..64 {
            match self.children.get(current) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        Some(current.clone())
    }

    /// Title of a subagent session: its task title, else the session title.
    pub(super) fn subagent_title(&self, id: &str) -> String {
        let parent = self.children.get(id).and_then(|p| self.entries.get(p));
        parent
            .and_then(|e| e.subagents.iter().find(|s| s.session_id.0 == id))
            .map(|s| s.title.clone())
            .filter(|t| !t.is_empty())
            .or_else(|| self.titles.get(id).cloned())
            .unwrap_or_default()
    }

    /// The snapshot when it differs from the last one returned here (`None` = nothing to send).
    pub(super) fn changed(&mut self) -> Option<Vec<FleetSession>> {
        let next = self.snapshot();
        if self.shown.as_ref() == Some(&next) {
            return None;
        }
        self.shown = Some(next.clone());
        Some(next)
    }

    fn snapshot(&self) -> Vec<FleetSession> {
        let open = self.open.as_deref();
        self.order
            .iter()
            .filter_map(|id| {
                let entry = self.entries.get(id);
                let is_open = open == Some(id.as_str());
                let child = self.children.contains_key(id);
                let (subagents, parents) = self.descendants(id);
                let busy = entry.is_some_and(|e| e.running)
                    || subagents.iter().any(|s| s.status.is_active());
                if !(is_open || (busy && !child)) {
                    return None;
                }
                Some(FleetSession {
                    subagents,
                    parents,
                    id: id.clone(),
                    title: self.titles.get(id).cloned().unwrap_or_default(),
                    prompt: entry.map(|e| e.prompt.clone()).unwrap_or_default(),
                    agent: entry.map(|e| e.agent.clone()).unwrap_or_default(),
                    running: entry.is_some_and(|e| e.running),
                    open: is_open,
                })
            })
            .collect()
    }

    /// 修复：Orchestra 只画出会话的直接子代理；子代理再委派出的子代理（嵌套）不显示。
    /// Depth first list of every subagent under `id`, with each one's parent index in that list.
    fn descendants(&self, id: &str) -> (Vec<api::SubagentView>, Vec<Option<usize>>) {
        let children = |session: &str| {
            self.entries
                .get(session)
                .map(|e| e.subagents.iter().rev().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        let mut out: Vec<api::SubagentView> = Vec::new();
        let mut parents = Vec::new();
        let mut pending: Vec<(&api::SubagentView, Option<usize>, usize)> =
            children(id).into_iter().map(|s| (s, None, 0)).collect();
        while let Some((sub, parent, depth)) = pending.pop() {
            if depth > 8 || out.iter().any(|s| s.session_id == sub.session_id) {
                continue;
            }
            out.push(sub.clone());
            parents.push(parent);
            let index = out.len() - 1;
            for child in children(sub.session_id.as_str()) {
                pending.push((child, Some(index), depth + 1));
            }
        }
        (out, parents)
    }
}
