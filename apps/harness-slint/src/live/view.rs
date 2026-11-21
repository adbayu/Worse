use super::{
    display, set_trail, AppWindow, ComponentHandle, Focus, LiveLine, LiveUi, ModelRc, Path,
    SharedString, Target, VecModel, AFTER, BEFORE, MAX_BYTES, STATE,
};

/// Fills `LiveUi` for `target` (only while the Live tab is on screen; otherwise only the trail moves).
pub(super) fn render(window: &AppWindow, target: Option<&Target>) {
    set_trail(window);
    let live = window.global::<LiveUi>();
    if !live.get_active() {
        return;
    }
    let Some(target) = target else {
        live.set_path(SharedString::new());
        live.set_name(SharedString::new());
        live.set_action(SharedString::new());
        live.set_note(SharedString::new());
        live.set_lines(ModelRc::new(VecModel::<LiveLine>::default()));
        return;
    };
    let workspace = STATE.with(|cell| cell.borrow().workspace.clone());
    let shown_path = display(&workspace, &target.path);
    let name = target
        .path
        .file_name()
        .map_or_else(|| shown_path.clone(), |n| n.to_string_lossy().into_owned());
    let view = build(&target.path, &target.focus);
    live.set_path(shown_path.as_str().into());
    live.set_name(name.as_str().into());
    live.set_action(view.action.into());
    live.set_note(view.note.as_str().into());
    live.set_cols(
        i32::try_from(view.lines.iter().map(|l| cells(&l.text)).max().unwrap_or(0)).unwrap_or(0),
    );
    live.set_lines(ModelRc::new(VecModel::from(view.lines)));
    live.set_focus(i32::try_from(view.focus).unwrap_or(0));
    let epoch = STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.epoch = s.epoch.wrapping_add(1);
        s.epoch
    });
    live.set_epoch(epoch);
}

pub(super) struct View {
    pub(super) action: &'static str,
    pub(super) note: String,
    pub(super) lines: Vec<LiveLine>,
    /// Index into `lines` of the first marked line.
    pub(super) focus: usize,
}

fn build(path: &Path, focus: &Focus) -> View {
    let text = match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_BYTES => {
            return note_only("file too large to show");
        }
        Ok(_) => std::fs::read(path)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned()),
        Err(_) => None,
    };
    let lang = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match (text, focus) {
        // Edit not on disk (yet): show the incoming lines on their own, marked "about to change".
        (None, Focus::Edit { new, .. }) => snippet(new, &lang, "file not on disk yet"),
        (None, _) => note_only("file not found"),
        (Some(text), focus) => {
            let file: Vec<&str> = text.lines().collect();
            let (action, from, to, note) = match focus {
                Focus::Read { from, to } => {
                    let to = if *to >= *from { *to } else { *from };
                    (
                        "read",
                        *from as usize,
                        to as usize,
                        lines_note(*from as usize, to as usize),
                    )
                }
                Focus::Open { line } => ("open", *line as usize, *line as usize, String::new()),
                Focus::Edit { new, old, line } => match find_block(&file, new) {
                    Some(start) => {
                        let end = start + new.lines().count().max(1) - 1;
                        ("edit", start, end, lines_note(start, end))
                    }
                    None => match old.as_deref().and_then(|o| find_block(&file, o)) {
                        Some(start) => {
                            let end =
                                start + old.as_deref().map_or(1, |o| o.lines().count().max(1)) - 1;
                            ("pending", start, end, lines_note(start, end))
                        }
                        None if *line > 0 => {
                            ("edit", *line as usize, *line as usize, String::new())
                        }
                        None => return snippet(new, &lang, "not found in the file"),
                    },
                },
            };
            window_view(&file, &lang, action, from, to, note)
        }
    }
}

/// Width of a line in character cells (wide East Asian characters take two).
fn cells(text: &str) -> usize {
    text.chars()
        .map(|c| if u32::from(c) >= 0x1100 { 2 } else { 1 })
        .sum()
}

fn lines_note(from: usize, to: usize) -> String {
    if from == 0 {
        String::new()
    } else if to > from {
        format!("lines {from}–{to}")
    } else {
        format!("line {from}")
    }
}

fn note_only(note: &str) -> View {
    View {
        action: "open",
        note: note.to_owned(),
        lines: Vec::new(),
        focus: 0,
    }
}

/// 1-based line where `block` first appears in `file`, all of its lines in order. A block of blank lines
/// only matches nothing (it would match almost anywhere).
pub(super) fn find_block(file: &[&str], block: &str) -> Option<usize> {
    let want: Vec<&str> = block.lines().collect();
    if want.iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    file.windows(want.len())
        .position(|w| w == want.as_slice())
        .map(|i| i + 1)
}

fn line_row(no: usize, text: &str, lang: &str, mark: i32) -> LiveLine {
    let shown = crate::markdown::expand_tabs(text);
    let light = crate::highlight::styled(&shown, lang, false);
    let dark = crate::highlight::styled(&shown, lang, true);
    let highlighted = light.is_some() && dark.is_some();
    LiveLine {
        no: i32::try_from(no).unwrap_or(0),
        text: shown.as_ref().into(),
        code_light: light.unwrap_or_default(),
        code_dark: dark.unwrap_or_default(),
        highlighted,
        mark,
    }
}

pub(super) fn window_view(
    file: &[&str],
    lang: &str,
    action: &'static str,
    from: usize,
    to: usize,
    note: String,
) -> View {
    let from = from.clamp(1, file.len().max(1));
    let to = to.clamp(from, file.len().max(from));
    let start = from.saturating_sub(BEFORE).max(1);
    let end = (to + AFTER).min(file.len());
    let mark = match action {
        "edit" => 2,
        "pending" => 3,
        "read" => 1,
        _ => 0,
    };
    let lines = (start..=end)
        .map(|no| {
            let m = if (from..=to).contains(&no) && mark != 0 {
                mark
            } else {
                0
            };
            line_row(no, file.get(no - 1).copied().unwrap_or_default(), lang, m)
        })
        .collect();
    View {
        action,
        note,
        lines,
        focus: from - start,
    }
}

fn snippet(new: &str, lang: &str, note: &str) -> View {
    View {
        action: "pending",
        note: note.to_owned(),
        lines: new
            .lines()
            .enumerate()
            .map(|(i, l)| line_row(i + 1, l, lang, 3))
            .collect(),
        focus: 0,
    }
}
