use super::{
    command, drop_hover, dropped, mpsc, open_path, paste_image, pick_files, run, select,
    whiteboard, with_state, AppWindow, ChatState, ChatUi, ComponentHandle, FileReq, ModelRc, Path,
    Rc, SharedString, VecModel, STATE,
};

pub fn install(
    window: &AppWindow,
    files_tx: mpsc::UnboundedSender<FileReq>,
    workspace: &Path,
    data_dir: &Path,
) {
    let palette_rows = Rc::new(VecModel::default());
    let mention_rows = Rc::new(VecModel::default());
    let chips = Rc::new(VecModel::default());
    let ui = window.global::<ChatUi>();
    ui.set_palette_rows(ModelRc::from(palette_rows.clone()));
    ui.set_mention_rows(ModelRc::from(mention_rows.clone()));
    ui.set_chips(ModelRc::from(chips.clone()));
    let state = ChatState {
        window: window.clone_strong(),
        files_tx,
        workspace: workspace.to_path_buf(),
        exports: data_dir.join("exports"),
        files: None,
        walk_sent: false,
        palette_rows,
        palette_targets: Vec::new(),
        palette_query: (String::new(), "all".into()),
        recent: Vec::new(),
        mention_rows,
        mention_targets: Vec::new(),
        mention_start: 0,
        chips,
        chip_data: Vec::new(),
        agent_commands: Vec::new(),
        skills: Vec::new(),
        attachments: data_dir.join("attachments"),
        last_draft: String::new(),
        history: Vec::new(),
        history_pos: None,
        history_stash: String::new(),
        sessions: Vec::new(),
        active: None,
        nav: Vec::new(),
        nav_pos: 0,
        nav_jump: None,
        find_query: String::new(),
        find_hits: Vec::new(),
        find_pos: 0,
        toast_timer: slint::Timer::default(),
    };
    STATE.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            *guard = Some(state);
        }
    });
    wire(window);
}

fn index(i: i32) -> usize {
    usize::try_from(i).unwrap_or(usize::MAX)
}

fn wire(window: &AppWindow) {
    let ui = window.global::<ChatUi>();
    ui.on_palette_step(|delta| {
        with_state(|s| s.palette_step(delta));
    });
    ui.on_palette_query(|query, scope| {
        with_state(|s| s.palette_query(&query, &scope));
    });
    ui.on_palette_run(|i| {
        if let Some(effect) = with_state(|s| s.palette_run(index(i))) {
            run(effect);
        }
    });
    ui.on_find(|query| {
        with_state(|s| s.find(&query));
    });
    ui.on_find_step(|delta| {
        with_state(|s| s.find_step(delta));
    });
    ui.on_find_close(|| {
        with_state(ChatState::close_find);
    });
    ui.on_draft_edited(|draft| {
        with_state(|s| s.draft_edited(&draft));
    });
    ui.on_mention_pick(|i| {
        if let Some(effect) = with_state(|s| s.mention_pick(index(i))) {
            run(effect);
        }
    });
    ui.on_remove_chip(|i| {
        with_state(|s| {
            let i = index(i);
            if i < s.chip_data.len() {
                s.chip_data.remove(i);
                s.chips.remove(i);
            }
        });
    });
    ui.on_history(|delta| with_state(|s| s.history(delta)).unwrap_or(false));
    ui.on_nav(|how| {
        if let Some(effect) = with_state(|s| s.nav(&how)) {
            run(effect);
        }
    });
    ui.on_export(|format| {
        with_state(|s| s.export(&format));
    });
    ui.on_reveal(|| command("revealWorkspace"));
    ui.on_copy_session_id(|id| {
        with_state(|s| {
            let id = if id.is_empty() {
                s.active.clone()
            } else {
                Some(id.to_string())
            };
            if let Some(id) = id {
                s.copy(&id);
            }
        });
    });
    ui.on_copy(|text| {
        with_state(|s| s.copy(&text));
    });
    ui.on_link_at(|links, offset| crate::markdown::link_at(&links, offset));
    ui.on_dismiss_error(|| {
        with_state(|s| s.window.set_error_text(SharedString::new()));
    });
    ui.on_attach_files(pick_files);
    whiteboard::install(window);
    ui.on_open_path(|path| open_path(&path));
    ui.on_show_earlier(crate::bridge::show_earlier);
    ui.on_paste_image(paste_image);
    install_drop(window);
}

/// OS file drops (Slint's winit backend does not forward them as Slint events), and window visibility for the
/// glyph art: it stops stepping while the window is minimized or fully covered (`ArtClock.visible`), so a
/// hidden window does no animation work at all. Nothing changes on screen while the window can be seen.
fn install_drop(window: &AppWindow) {
    use slint::winit_030::{winit::event::WindowEvent, EventResult, WinitWindowAccessor};
    let weak = window.as_weak();
    let set_visible = move |visible: bool| {
        if let Some(window) = weak.upgrade() {
            let clock = window.global::<crate::ui::ArtClock>();
            if clock.get_visible() != visible {
                clock.set_visible(visible);
            }
        }
    };
    let weak = window.as_weak();
    let set_focused = move |focused: bool| {
        if let Some(window) = weak.upgrade() {
            let clock = window.global::<crate::ui::ArtClock>();
            if clock.get_focused() != focused {
                clock.set_focused(focused);
            }
        }
    };
    select::install(window, |text| {
        with_state(|s| s.copy(text));
    });
    let weak = window.as_weak();
    window.window().on_winit_window_event(move |shell, event| {
        match event {
            // Where the pointer is while a text selection is dragged from one block of the chat into the
            // next (`select`).
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(window) = weak.upgrade() {
                    #[allow(clippy::cast_possible_truncation)]
                    select::moved(
                        &window,
                        (position.y / f64::from(shell.scale_factor())) as f32,
                    );
                }
            }
            WindowEvent::MouseInput {
                state,
                button: slint::winit_030::winit::event::MouseButton::Left,
                ..
            } => {
                if let Some(window) = weak.upgrade() {
                    select::button(&window, state.is_pressed());
                }
            }
            WindowEvent::HoveredFile(_) => drop_hover(true),
            WindowEvent::HoveredFileCancelled => drop_hover(false),
            WindowEvent::DroppedFile(path) => dropped(path),
            WindowEvent::Occluded(occluded) => set_visible(!occluded),
            // Windows reports a minimized window as a 0x0 resize.
            WindowEvent::Resized(size) => set_visible(size.width > 0 && size.height > 0),
            // Some window systems report "occluded" wrongly (X11 without a window manager does at startup).
            // A window that has the focus or the pointer is on screen.
            WindowEvent::CursorEntered { .. } => set_visible(true),
            WindowEvent::Focused(focused) => {
                if *focused {
                    set_visible(true);
                }
                set_focused(*focused);
            }
            _ => {}
        }
        EventResult::Propagate
    });
}
