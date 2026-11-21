//! Live code view (Side Pane "Live" tab) and the file / line-range facts of tool rows.
//!
//! While a turn runs, every tool call that reads or edits a file becomes the "latest" target. With follow on,
//! the Live view shows it: a window of the file around the lines the agent read or wrote, those lines marked.
//! The file is read from disk only while the Live tab is on screen (`LiveUi.active`); otherwise only the target
//! is remembered. Agents send an edit as one old/new pair (no keystrokes), so the view marks the written lines
//! once and lets the highlight settle; nothing is animated character by character.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use harness_core_api::{ToolContent, ToolLocation};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::ui::{AppWindow, LiveFile, LiveLine, LiveUi};

mod rows;
mod view;

use rows::read_span;
pub(crate) use rows::row_file;
use view::render;

/// Lines shown above and below the focus line (the view is a window of the file, not the whole file).
const BEFORE: usize = 80;
const AFTER: usize = 200;
/// Larger files are not shown (the view says so).
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Files remembered in the turn trail.
const TRAIL: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Focus {
    /// The agent read lines `from..=to` (1-based); `to == 0` means "around `from`".
    Read { from: u32, to: u32 },
    /// The agent wrote `new` (replacing `old` when known); `line` is the agent's own location, if any.
    Edit {
        new: String,
        old: Option<String>,
        line: u32,
    },
    /// The user opened the file from a chat chip.
    Open { line: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    path: PathBuf,
    focus: Focus,
}

#[derive(Default)]
struct State {
    workspace: PathBuf,
    /// What the view shows.
    shown: Option<Target>,
    /// The agent's most recent target (shown again when follow is turned back on).
    latest: Option<Target>,
    trail: Vec<PathBuf>,
    epoch: i32,
    open_tick: i32,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

/// Callbacks of the Live view. `workspace` resolves relative tool paths.
pub(crate) fn install(window: &AppWindow) {
    let live_ui = window.global::<LiveUi>();
    let weak = window.as_weak();
    live_ui.on_open(move |path, line| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let target = STATE.with(|cell| {
            let mut s = cell.borrow_mut();
            let path = resolve(&s.workspace, &path);
            let line = u32::try_from(line).unwrap_or(0);
            // The agent's own target for this file carries the exact lines; prefer it.
            let focus = match &s.latest {
                Some(t) if t.path == path => t.focus.clone(),
                _ => Focus::Open { line },
            };
            let target = Target { path, focus };
            s.shown = Some(target.clone());
            s.open_tick += 1;
            target
        });
        let tick = STATE.with(|cell| cell.borrow().open_tick);
        window.global::<LiveUi>().set_follow(false);
        window.global::<LiveUi>().set_view("code".into());
        render(&window, Some(&target));
        window.global::<LiveUi>().set_open_tick(tick);
    });
    let weak = window.as_weak();
    live_ui.on_set_follow(move |follow| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        window.global::<LiveUi>().set_follow(follow);
        if follow {
            let latest = STATE.with(|cell| {
                let mut s = cell.borrow_mut();
                let latest = s.latest.clone();
                s.shown.clone_from(&latest);
                latest
            });
            render(&window, latest.as_ref());
        }
    });
    let weak = window.as_weak();
    live_ui.on_refresh(move || {
        if let Some(window) = weak.upgrade() {
            let shown = STATE.with(|cell| cell.borrow().shown.clone());
            render(&window, shown.as_ref());
        }
    });
}

/// Brings the Live tab to the front without changing what it shows (the Screen view asks for this when the
/// agent starts using the computer).
pub(crate) fn request_open(window: &AppWindow) {
    let tick = STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.open_tick += 1;
        s.open_tick
    });
    window.global::<LiveUi>().set_open_tick(tick);
}

/// The session's workspace changed (relative tool paths resolve against it).
pub(crate) fn set_workspace(workspace: &Path) {
    STATE.with(|cell| cell.borrow_mut().workspace = workspace.to_path_buf());
}

/// A new user turn: the trail starts over.
pub(crate) fn new_turn(window: &AppWindow) {
    STATE.with(|cell| cell.borrow_mut().trail.clear());
    window
        .global::<LiveUi>()
        .set_trail(ModelRc::new(VecModel::<LiveFile>::default()));
}

/// The session was left or switched: nothing is shown until the agent touches a file again.
pub(crate) fn clear(window: &AppWindow) {
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.shown = None;
        s.latest = None;
        s.trail.clear();
    });
    window.global::<LiveUi>().set_who(SharedString::new());
    render(window, None);
}

/// A running tool call reported content or locations: remember it as the latest target and, when following,
/// show it.
pub(crate) fn touch(
    window: &AppWindow,
    kind: Option<&str>,
    content: &[ToolContent],
    locations: &[ToolLocation],
) {
    touch_as(window, "", kind, content, locations);
}

/// A subagent under the open session read or edited a file: moves the view there only while the Live tab
/// follows subagents too.
pub(crate) fn touch_subagent(
    window: &AppWindow,
    who: &str,
    kind: Option<&str>,
    content: &[ToolContent],
    locations: &[ToolLocation],
) {
    if window.global::<LiveUi>().get_follow_subagents() {
        let who = if who.trim().is_empty() {
            "Subagent"
        } else {
            who.trim()
        };
        touch_as(window, who, kind, content, locations);
    }
}

fn touch_as(
    window: &AppWindow,
    who: &str,
    kind: Option<&str>,
    content: &[ToolContent],
    locations: &[ToolLocation],
) {
    let Some(target) =
        STATE.with(|cell| target_of(&cell.borrow().workspace, kind, content, locations))
    else {
        return;
    };
    let follow = window.global::<LiveUi>().get_follow();
    if follow {
        window.global::<LiveUi>().set_who(who.into());
    }
    // A read reports twice (started, finished) with the same span and does not change the file: the
    // view already shows it.
    let same = STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        let same = matches!(target.focus, Focus::Read { .. }) && s.shown.as_ref() == Some(&target);
        s.trail.retain(|p| *p != target.path);
        s.trail.push(target.path.clone());
        let extra = s.trail.len().saturating_sub(TRAIL);
        s.trail.drain(..extra);
        s.latest = Some(target.clone());
        if follow {
            s.shown = Some(target.clone());
        }
        same
    });
    if follow {
        // Following the agent: it is in a file now, not at the screen.
        window.global::<LiveUi>().set_view("code".into());
    }
    if follow && !same {
        render(window, Some(&target));
    } else {
        set_trail(window);
    }
}

fn target_of(
    workspace: &Path,
    kind: Option<&str>,
    content: &[ToolContent],
    locations: &[ToolLocation],
) -> Option<Target> {
    for item in content {
        if let ToolContent::Diff {
            path,
            old_text,
            new_text,
        } = item
        {
            let line = locations
                .iter()
                .find(|l| l.path == *path)
                .and_then(|l| l.line)
                .unwrap_or(0);
            return Some(Target {
                path: resolve(workspace, path),
                focus: Focus::Edit {
                    new: new_text.clone(),
                    old: old_text.clone(),
                    line,
                },
            });
        }
    }
    let location = locations.first()?;
    if kind.is_some_and(|k| !matches!(k, "read" | "edit" | "search")) {
        return None;
    }
    let (from, to) = read_span(content, location.line);
    Some(Target {
        path: resolve(workspace, &location.path),
        focus: Focus::Read { from, to },
    })
}

fn resolve(workspace: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() || workspace.as_os_str().is_empty() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    }
}

fn display(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn set_trail(window: &AppWindow) {
    let (trail, shown) = STATE.with(|cell| {
        let s = cell.borrow();
        (s.trail.clone(), s.shown.as_ref().map(|t| t.path.clone()))
    });
    let rows: Vec<LiveFile> = trail
        .iter()
        .map(|p| LiveFile {
            path: p.to_string_lossy().as_ref().into(),
            name: p
                .file_name()
                .map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy())
                .as_ref()
                .into(),
            current: shown.as_ref() == Some(p),
        })
        .collect();
    window
        .global::<LiveUi>()
        .set_trail(ModelRc::new(VecModel::from(rows)));
}

#[cfg(test)]
mod tests {
    use super::rows::RowFile;
    use super::view::{find_block, window_view};
    use super::*;

    fn text(t: &str) -> ToolContent {
        ToolContent::Text { text: t.to_owned() }
    }

    #[test]
    fn read_span_uses_numbered_output() {
        let out = text("   120→fn a() {\n   121→}\n   122→");
        assert_eq!(read_span(&[out], Some(120)), (120, 122));
        let tabs = text("    7\tlet x = 1;\n    8\tlet y = 2;");
        assert_eq!(read_span(&[tabs], None), (7, 8));
    }

    #[test]
    fn read_span_falls_back_to_location_and_line_count() {
        assert_eq!(read_span(&[text("a\nb\nc")], Some(40)), (40, 42));
        assert_eq!(read_span(&[text("a\nb")], None), (1, 2));
        assert_eq!(read_span(&[], None), (0, 0));
    }

    #[test]
    fn row_file_for_edit_and_read() {
        let diff = ToolContent::Diff {
            path: "src/lib.rs".into(),
            old_text: Some("a".into()),
            new_text: "b\nc".into(),
        };
        let loc = |line| ToolLocation {
            path: "src/lib.rs".into(),
            line,
        };
        assert_eq!(
            row_file(Some("edit"), std::slice::from_ref(&diff), &[loc(Some(10))]).range,
            "L10–11"
        );
        assert_eq!(row_file(Some("edit"), &[diff], &[]).range, "+2 −1");
        let read = row_file(Some("read"), &[text("   5→x\n   6→y")], &[loc(None)]);
        assert_eq!(
            (read.path.as_str(), read.line, read.range.as_str()),
            ("src/lib.rs", 5, "L5–6")
        );
        assert_eq!(
            row_file(Some("execute"), &[text("ok")], &[]),
            RowFile::default()
        );
    }

    #[test]
    fn find_block_matches_whole_block() {
        let file = ["fn a() {", "    x();", "}", "fn b() {", "    x();", "}"];
        assert_eq!(find_block(&file, "fn b() {\n    x();"), Some(4));
        assert_eq!(find_block(&file, "    x();\n}"), Some(2));
        assert_eq!(find_block(&file, "missing"), None);
        assert_eq!(find_block(&file, "\n\n"), None);
    }

    #[test]
    fn window_marks_the_focus_lines() {
        let file: Vec<String> = (1..=500).map(|n| format!("line {n}")).collect();
        let refs: Vec<&str> = file.iter().map(String::as_str).collect();
        let view = window_view(&refs, "", "read", 200, 202, String::new());
        assert_eq!(view.lines.first().map(|l| l.no), Some(120));
        assert_eq!(view.lines.len(), 402 - 120 + 1);
        assert_eq!(view.lines[view.focus].no, 200);
        let marked: Vec<i32> = view
            .lines
            .iter()
            .filter(|l| l.mark == 1)
            .map(|l| l.no)
            .collect();
        assert_eq!(marked, vec![200, 201, 202]);
    }
}
