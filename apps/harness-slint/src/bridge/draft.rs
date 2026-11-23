//! New session draft, modelled on the Antigravity desktop app: "New session" leaves the open session for an
//! empty composer that shows the project and checkout (`ui/chat/draft_chips.slint`); the session is created
//! with the first message. The project defaults to the open session's project, else the last one chosen.
//! "No project" sessions run in `<HARNESS_HOME>/scratch` (Antigravity uses its install folder, which an
//! update would wipe) and the sidebar lists them under Conversations.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, VecModel};

use super::{with_state, AppWindow, UiState};
use crate::ui::{DraftProject, DraftUi};

/// Picker size: recent projects shown above "No project".
const RECENT: usize = 10;

/// Folder of "No project" sessions.
pub(super) fn scratch_dir() -> PathBuf {
    crate::startup::harness_home().join("scratch")
}

fn norm(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// True for the scratch folder of "No project" sessions.
pub(super) fn is_scratch(path: &str) -> bool {
    !path.is_empty() && norm(Path::new(path)) == norm(&scratch_dir())
}

fn folder_name(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

/// Project a new draft starts in.
pub(super) enum DraftTarget {
    /// The open session's project, or the last chosen one when nothing is open.
    Current,
    /// No project: the scratch folder.
    NoProject,
    Folder(PathBuf),
}

/// Project and checkout of the next new session.
#[derive(Default)]
pub(super) struct Draft {
    /// `None` = No project (scratch folder).
    pub(super) project: Option<PathBuf>,
    pub(super) worktree: bool,
}

impl Draft {
    pub(super) fn new(workspace: &Path) -> Self {
        let path = workspace.to_string_lossy();
        Self {
            project: (!path.is_empty() && !is_scratch(&path)).then(|| workspace.to_path_buf()),
            worktree: false,
        }
    }
}

impl UiState {
    /// Folder of the open session's project. Uses the project key rather than `workspace_path`: a worktree
    /// session's path lies under `~/.harness/worktrees`. A key that is not a folder falls back to the path.
    pub(super) fn active_project(&self) -> Option<PathBuf> {
        let active = self.active.as_ref()?;
        let summary = self.summaries.iter().find(|s| &s.id == active)?;
        let project = PathBuf::from(&summary.project);
        if !summary.project.is_empty() && project.is_dir() {
            return Some(project);
        }
        (!summary.workspace_path.is_empty()).then(|| PathBuf::from(&summary.workspace_path))
    }

    /// Folder the draft's session starts in (creates the scratch folder for "No project").
    pub(super) fn draft_target(&self) -> PathBuf {
        if let Some(project) = &self.draft.project {
            return project.clone();
        }
        let scratch = scratch_dir();
        if let Err(error) = std::fs::create_dir_all(&scratch) {
            eprintln!(
                "harness-slint: cannot create {}: {error}",
                scratch.display()
            );
        }
        scratch
    }

    /// "New session": an empty draft in `project`.
    pub(super) fn start_draft(&mut self, worktree: bool, project: DraftTarget) {
        match project {
            DraftTarget::Folder(dir) => self.draft.project = Some(dir),
            DraftTarget::NoProject => self.draft.project = None,
            DraftTarget::Current => {
                if let Some(open) = self.active_project() {
                    let scratch = is_scratch(&open.to_string_lossy());
                    self.draft.project = (!scratch).then_some(open);
                }
            }
        }
        self.draft.worktree = worktree && self.draft.project.is_some();
        if self.active.is_some() {
            self.clear_session();
            self.opened_title.clear();
            self.refresh_sessions();
        }
        self.sync_draft();
    }

    fn pick_draft(&mut self, choice: &str) {
        match choice {
            "" => {
                self.draft.project = None;
                self.draft.worktree = false;
            }
            "local" => self.draft.worktree = false,
            "worktree" => self.draft.worktree = self.draft.project.is_some(),
            path => self.draft.project = Some(PathBuf::from(path)),
        }
        self.sync_draft();
    }

    /// Recent projects of the sidebar sessions, newest first, without the scratch folder.
    fn recent_projects(&self) -> Vec<DraftProject> {
        let mut seen: Vec<(u64, String)> = Vec::new();
        for s in &self.summaries {
            let key = if s.project.is_empty() {
                &s.workspace_path
            } else {
                &s.project
            };
            if key.is_empty() || is_scratch(key) || is_scratch(&s.workspace_path) {
                continue;
            }
            match seen
                .iter_mut()
                .find(|(_, k)| norm(Path::new(k)) == norm(Path::new(key)))
            {
                Some(entry) => entry.0 = entry.0.max(s.updated_ms),
                None => seen.push((s.updated_ms, key.clone())),
            }
        }
        seen.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        let mut out: Vec<DraftProject> = seen
            .into_iter()
            .filter(|(_, path)| Path::new(path).is_dir())
            .take(RECENT)
            .map(|(_, path)| DraftProject {
                label: folder_name(&path).into(),
                path: path.into(),
            })
            .collect();
        if let Some(project) = &self.draft.project {
            let path = project.to_string_lossy();
            if !out
                .iter()
                .any(|p| norm(Path::new(p.path.as_str())) == norm(project))
            {
                out.insert(
                    0,
                    DraftProject {
                        label: folder_name(&path).into(),
                        path: path.as_ref().into(),
                    },
                );
            }
        }
        out
    }

    /// Mirrors the draft into `DraftUi` (shown while no session is open).
    pub(super) fn sync_draft(&self) {
        let ui = self.window.global::<DraftUi>();
        ui.set_active(self.active.is_none());
        let path = self
            .draft
            .project
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        ui.set_project(folder_name(&path).into());
        ui.set_project_path(path.as_str().into());
        ui.set_worktree(self.draft.worktree);
        ui.set_projects(ModelRc::from(Rc::new(VecModel::from(
            self.recent_projects(),
        ))));
    }
}

/// Wires the chip picker. "Open folder…" runs the window's folder dialog, outside the state borrow.
pub(super) fn wire(window: &AppWindow) {
    let weak = window.as_weak();
    window.global::<DraftUi>().on_pick(move |choice| {
        if choice == "open" {
            if let Some(window) = weak.upgrade() {
                window.invoke_open_folder();
            }
            return;
        }
        with_state(|s| s.pick_draft(&choice));
    });
    with_state(|s| s.sync_draft());
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn scratch_folder_is_recognised() {
        let scratch = scratch_dir();
        assert!(is_scratch(&scratch.to_string_lossy()));
        assert!(is_scratch(&format!("{}/", scratch.to_string_lossy())));
        assert!(!is_scratch(""));
        assert!(!is_scratch("D:/kaze-last/shym4view"));
        assert_eq!(folder_name("D:\\kaze-last\\shym4view"), "shym4view");
        assert!(Draft::new(&scratch).project.is_none());
    }
}
