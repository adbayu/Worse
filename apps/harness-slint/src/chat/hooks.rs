use super::{sv, with_state, Chip, ContextItem, MentionCandidate, Model, Path, PathBuf};

/// Sidebar order and open session changed (bridge `refresh_sessions`).
pub fn sessions_changed(list: Vec<(String, String)>, active: Option<&str>) {
    with_state(|s| {
        s.sessions = list;
        if s.active.as_deref() != active {
            if let Some(id) = active {
                s.session_opened(id);
            } else {
                s.active = None;
                s.close_find();
            }
        }
        s.refresh_nav();
    });
}

/// 修复：@ 文件提及、打开路径、拖放仍用启动时的工作区；随打开的会话切换，并丢弃旧工作区的文件列表。
pub fn set_workspace(path: &Path) {
    with_state(|s| {
        if s.workspace != path {
            s.workspace = path.to_path_buf();
            s.files = None;
            s.walk_sent = false;
        }
    });
}

/// Message about to be sent: pasted text folded in, the other chips as context items, prompt history recorded.
pub fn prepare_send(text: &str) -> (String, Vec<ContextItem>) {
    with_state(|s| s.prepare_send(text)).unwrap_or_else(|| (text.to_string(), Vec::new()))
}

/// Path and label of the image chip at `index` (whiteboard annotation).
pub(super) fn chip_image(index: usize) -> Option<(String, String)> {
    with_state(
        |s| match (s.chip_data.get(index), s.chips.row_data(index)) {
            (Some(Chip::Image { path, .. }), Some(row)) => {
                let full = if Path::new(path).is_absolute() {
                    PathBuf::from(path)
                } else {
                    s.workspace.join(path)
                };
                Some((full.to_string_lossy().into_owned(), row.label.to_string()))
            }
            _ => None,
        },
    )
    .flatten()
}

/// Removes the image chip whose file is `full` (the annotated original). Looked up by path, not index: chips can be
/// removed while the whiteboard is open.
pub(super) fn remove_image_chip(full: &str) {
    with_state(|s| {
        let found = s.chip_data.iter().position(|c| match c {
            Chip::Image { path, .. } => {
                let resolved = if Path::new(path).is_absolute() {
                    PathBuf::from(path)
                } else {
                    s.workspace.join(path)
                };
                resolved.to_string_lossy() == full
            }
            _ => false,
        });
        if let Some(index) = found {
            s.chip_data.remove(index);
            s.chips.remove(index);
        }
    });
}

pub(super) fn attachments_dir() -> Option<PathBuf> {
    with_state(|s| s.attachments.clone())
}

/// Short confirmation toast; `id` is a `ChatUi.action-label` id (translated in Slint).
pub fn toast_label(id: &str) {
    with_state(|s| {
        let label = s.ui().invoke_action_label(sv(id));
        s.toast(&label);
    });
}

/// Short confirmation toast with an already-formatted message.
pub fn toast_text(message: &str) {
    with_state(|s| s.toast(message));
}

/// Attaches a Work-item element comment chip to the composer (W6).
pub fn add_work_comment(path: &str, comment: &str) {
    with_state(|s| {
        s.add_work_comment(path, comment);
        s.toast("Comment added to composer");
    });
}

/// "Open in editor" of a tool-card path (`path` or `path:line`): the OS default app on its own thread.
/// `HARNESS_OPENER_LOG` (gates only) appends the resolved path to that file instead of launching an app.
pub fn open_path(raw: &str) {
    let trimmed = raw.trim();
    let workspace = with_state(|s| s.workspace.clone());
    let literal = if Path::new(trimmed).is_absolute() {
        Some(PathBuf::from(trimmed))
    } else {
        workspace
            .as_deref()
            .map(|workspace| workspace_path(workspace, trimmed))
    };
    let path = if literal.as_ref().is_some_and(|path| path.exists()) {
        literal.unwrap_or_default()
    } else {
        let trimmed = strip_location_suffix(trimmed);
        if Path::new(trimmed).is_absolute() {
            PathBuf::from(trimmed)
        } else if let Some(workspace) = workspace {
            workspace_path(&workspace, trimmed)
        } else {
            return;
        }
    };
    if !path.exists() {
        return;
    }
    std::thread::spawn(move || {
        let result = match std::env::var_os("HARNESS_OPENER_LOG") {
            Some(log) => {
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log)
                    .and_then(|mut f| writeln!(f, "{}", path.display()))
            }
            None => open_with_os(&path),
        };
        if let Err(error) = result {
            let message = error.to_string();
            let _ = slint::invoke_from_event_loop(move || {
                with_state(|s| s.window.set_error_text(sv(&message)));
            });
        }
    });
}

fn workspace_path(workspace: &Path, relative: &str) -> PathBuf {
    // Agent paths use forward slashes; join components for the current platform.
    relative
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .fold(workspace.to_path_buf(), |acc, part| acc.join(part))
}

fn strip_location_suffix(mut path: &str) -> &str {
    loop {
        let Some((head, suffix)) = path.rsplit_once(':') else {
            break;
        };
        if head.len() == 1 && head.as_bytes()[0].is_ascii_alphabetic() {
            break;
        }
        let suffix = suffix.strip_prefix('L').unwrap_or(suffix);
        let is_line = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
        let is_location = if let Some((first, last)) = suffix.split_once('-') {
            is_line(first.strip_prefix('L').unwrap_or(first))
                && is_line(last.strip_prefix('L').unwrap_or(last))
        } else {
            is_line(suffix)
        };
        if is_location {
            path = head;
        } else {
            break;
        }
    }
    path
}

/// Opens a Markdown link when it points to a local path or `file://` URI.
pub fn open_link(raw: &str) {
    let target = raw.trim();
    if target.is_empty() || target.starts_with('#') {
        return;
    }

    let mut target = target.to_owned();
    if let Ok(url) = url::Url::parse(&target) {
        if url.scheme() == "file" {
            let host = url.host_str();
            let drive_host = cfg!(windows)
                && host.is_some_and(|host| {
                    host.len() == 2
                        && host.as_bytes()[0].is_ascii_alphabetic()
                        && host.as_bytes()[1] == b':'
                });
            if host.is_some_and(|host| !host.is_empty() && !host.eq_ignore_ascii_case("localhost"))
                && !drive_host
            {
                return;
            }
            if drive_host {
                let Some(drive) = host.and_then(|host| host.chars().next()) else {
                    return;
                };
                let file_url = format!("file:///{drive}:{}", url.path());
                let Some(path) = url::Url::parse(&file_url)
                    .ok()
                    .and_then(|url| url.to_file_path().ok())
                else {
                    return;
                };
                open_path(&path.to_string_lossy());
                return;
            }
            if let Ok(path) = url.to_file_path() {
                open_path(&path.to_string_lossy());
                return;
            }
            // `file:relative/path` is not an absolute file URI; resolve it against the workspace below.
            target = url.path().trim_start_matches('/').to_owned();
        } else if target.contains("://") {
            // External URLs are not local files.
            return;
        }
    }

    #[cfg(windows)]
    {
        if target.starts_with(r"\\") || target.starts_with("//") {
            return;
        }
        if target.starts_with('/')
            && target
                .as_bytes()
                .get(1)
                .is_some_and(|byte| byte.is_ascii_alphabetic())
            && target.as_bytes().get(2) == Some(&b':')
        {
            target.remove(0);
        } else if target.starts_with('/') {
            target.remove(0);
        }
    }

    let workspace = with_state(|s| s.workspace.clone());
    if (Path::new(&target).is_absolute() && Path::new(&target).exists())
        || workspace
            .as_deref()
            .is_some_and(|workspace| workspace_path(workspace, &target).exists())
    {
        open_path(&target);
        return;
    }

    let target = strip_location_suffix(&target);

    #[cfg(windows)]
    if target.len() > 2
        && target.as_bytes()[0].is_ascii_alphabetic()
        && target.as_bytes()[1] == b':'
        && !matches!(target.as_bytes()[2], b'/' | b'\\')
    {
        if let Ok(path) = Path::new(target).canonicalize() {
            open_path(&path.to_string_lossy());
        }
        return;
    }

    if Path::new(target).is_absolute() {
        if Path::new(target).exists() {
            open_path(target);
            return;
        }
        let path = if target.contains('%') || target.contains('#') || target.contains('?') {
            absolute_file_url_path(target).unwrap_or_else(|| PathBuf::from(target))
        } else {
            PathBuf::from(target)
        };
        open_path(&path.to_string_lossy());
        return;
    }

    let Some(workspace) = workspace else {
        return;
    };
    if workspace_path(&workspace, target).exists() {
        open_path(target);
        return;
    }
    let Some(base) = url::Url::from_directory_path(workspace).ok() else {
        return;
    };
    let Ok(url) = base.join(&format!("./{target}")) else {
        return;
    };
    if url.scheme() == "file" {
        if let Ok(path) = url.to_file_path() {
            open_path(&path.to_string_lossy());
        }
    }
}

fn absolute_file_url_path(path: &str) -> Option<PathBuf> {
    let path = path.replace('\\', "/");
    let url = if cfg!(windows) && path.as_bytes().get(1) == Some(&b':') {
        format!("file:///{path}")
    } else {
        format!("file://{path}")
    };
    url::Url::parse(&url).ok()?.to_file_path().ok()
}

fn open_with_os(path: &Path) -> std::io::Result<()> {
    let mut command = if cfg!(windows) {
        let mut c = std::process::Command::new("explorer.exe");
        c.arg(path);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    // explorer.exe 成功打开时也可能返回非零退出码，这里只关心能否启动。
    command.spawn().map(|_| ())
}

/// A rewound turn's text and context back into the composer (`ChatCall::Rewind` result).
pub fn restore_composer(text: &str, items: Vec<ContextItem>) {
    with_state(|s| s.restore(text, items));
}

/// Skills and subagents of the open session's agent (`ChatCall::Mentions` with the skill kinds).
pub fn agent_mentions(list: Vec<MentionCandidate>) {
    with_state(|s| s.skills = list);
}

/// Slash commands of the open session's agent (`ChatCall::Commands`).
pub fn agent_commands(commands: Vec<(String, String)>) {
    with_state(|s| {
        s.agent_commands = commands;
        if s.ui().get_mention_open() {
            let draft = s.last_draft.clone();
            s.update_mention(&draft);
        }
    });
}

/// Adds picked / saved files as chips (images become image chips).
pub fn attach_paths(paths: Vec<PathBuf>) {
    for path in paths {
        dropped(&path);
    }
}

/// "Attach files": the OS file picker runs on its own thread so the UI keeps painting.
pub fn pick_files() {
    let start = with_state(|s| s.workspace.clone());
    std::thread::spawn(move || {
        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = start {
            dialog = dialog.set_directory(dir);
        }
        if let Some(paths) = dialog.pick_files() {
            let _ = slint::invoke_from_event_loop(move || attach_paths(paths));
        }
    });
}

/// Ctrl/Cmd+V with an image on the clipboard: saved as PNG under `<data>/attachments`, attached as an image chip.
/// Returns false when the clipboard holds no image (the text paste then proceeds).
pub fn paste_image() -> bool {
    let Some(png) = clipboard_png() else {
        return false;
    };
    let Some(dir) = with_state(|s| s.attachments.clone()) else {
        return false;
    };
    save_attachment(dir, png, "pasted");
    true
}

/// Writes PNG bytes to `<dir>/<prefix>-<uuid>.png` off the UI thread, then attaches the file.
pub fn save_attachment(dir: PathBuf, png: Vec<u8>, prefix: &str) {
    let path = dir.join(format!("{prefix}-{}.png", uuid::Uuid::new_v4().simple()));
    std::thread::spawn(move || {
        let result = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &png));
        let _ = slint::invoke_from_event_loop(move || match result {
            Ok(()) => attach_paths(vec![path]),
            Err(error) => {
                with_state(|s| s.window.set_error_text(sv(&error.to_string())));
            }
        });
    });
}

#[cfg(windows)]
fn clipboard_png() -> Option<Vec<u8>> {
    use clipboard_win::{formats, get_clipboard};
    let bmp: Vec<u8> = get_clipboard(formats::Bitmap).ok()?;
    let image = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp).ok()?;
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Image paste reads the Windows clipboard bitmap; other platforms attach images by drop or the file picker.
#[cfg(not(windows))]
fn clipboard_png() -> Option<Vec<u8>> {
    None
}

/// Files the OS dropped on the window (winit `DroppedFile`); paths inside the workspace become relative.
pub fn dropped(path: &Path) {
    with_state(|s| {
        s.ui().set_drop_hover(false);
        let shown = path.strip_prefix(&s.workspace).map_or_else(
            |_| path.to_string_lossy().into_owned(),
            |rel| rel.to_string_lossy().replace('\\', "/"),
        );
        s.add_file_chip(&shown);
    });
}

pub fn drop_hover(on: bool) {
    with_state(|s| s.ui().set_drop_hover(on));
}

/// Workspace walk reply (files worker).
pub fn files_walked(files: Vec<String>) {
    with_state(|s| {
        s.files = Some(files);
        if s.ui().get_palette_open() {
            let (query, scope) = s.palette_query.clone();
            s.palette_query(&query, &scope);
        }
        if s.ui().get_mention_open() {
            let draft = s.last_draft.clone();
            s.update_mention(&draft);
        }
    });
}

/// Export write reply (files worker).
pub fn exported(result: Result<PathBuf, String>) {
    with_state(|s| match result {
        Ok(path) => {
            let label = s.ui().invoke_action_label(sv("exported"));
            s.toast(&format!("{label} {}", path.display()));
        }
        Err(error) => s.window.set_error_text(sv(&error)),
    });
}
