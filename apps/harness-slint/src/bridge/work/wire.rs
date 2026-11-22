//! Slint callback wiring for `WorkLibraryUi` and `WorkUi`.

use std::path::PathBuf;

use slint::ComponentHandle;

use super::actions;
use crate::bridge::{with_state, UiState};
use crate::ui::{AppWindow, WorkLibraryUi, WorkUi};

fn current_root() -> Option<PathBuf> {
    let mut root = None;
    with_state(|s| root = Some(s.work_root()));
    root
}

pub(crate) fn wire(window: &AppWindow) {
    wire_library(window);
    wire_panel(window);
}

fn wire_library(window: &AppWindow) {
    let lib = window.global::<WorkLibraryUi>();
    lib.on_close(|| with_state(|s| s.work_lib_ui().set_open(false)));
    lib.on_refresh(|| with_state(|s| s.work_refresh_library()));
    lib.on_show(|| with_state(UiState::work_show_library));
    lib.on_open_item(|slug| with_state(|s| s.work_open(slug.to_string())));
    lib.on_filter_changed(|kind, query| {
        with_state(|s| {
            s.work.filter_kind = kind.to_string();
            s.work.search_query = query.to_string();
            s.work_apply_library_filter();
        });
    });
    lib.on_new_from_template(|template| {
        let Some(root) = current_root() else {
            return;
        };
        let tpl = template.to_string();
        let _ = std::thread::Builder::new()
            .name("work-create".into())
            .spawn(move || {
                let res = actions::create_from_template(&root, &tpl);
                let _ = slint::invoke_from_event_loop(move || match res {
                    Ok(slug) => with_state(|s| {
                        s.work_open(slug.clone());
                        crate::chat::toast_text(&format!("Created work/{slug}"));
                    }),
                    Err(e) => crate::chat::toast_text(&format!("Create failed: {e}")),
                });
            });
    });
    lib.on_import_file(|| {
        let Some(root) = current_root() else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("work-import".into())
            .spawn(move || {
                if let Some(res) = actions::pick_and_import(&root) {
                    let _ = slint::invoke_from_event_loop(move || match res {
                        Ok((slug, note)) => with_state(|s| {
                            s.work_open(slug);
                            crate::chat::toast_text(&note);
                        }),
                        Err(e) => crate::chat::toast_text(&format!("Import failed: {e}")),
                    });
                }
            });
    });
}

fn wire_panel(window: &AppWindow) {
    let ui = window.global::<WorkUi>();
    ui.on_open_item(|slug| with_state(|s| s.work_open(slug.to_string())));
    ui.on_back(|| with_state(UiState::work_show_library));
    ui.on_close(|| {
        with_state(|s| {
            s.work_ui().set_open(false);
            s.window.set_files_open(false);
        });
    });
    ui.on_refresh(|| {
        with_state(|s| {
            let slug = s.work.open_slug.clone();
            if !slug.is_empty() {
                s.work_load(slug, s.work.active_sheet);
            }
        });
    });
    ui.on_select_index(|idx| {
        with_state(|s| s.work_show_page(usize::try_from(idx).unwrap_or(0)));
    });
    ui.on_select_sheet(|idx| {
        with_state(|s| {
            let sheet = usize::try_from(idx).unwrap_or(0);
            let slug = s.work.open_slug.clone();
            if !slug.is_empty() {
                s.work.active_sheet = sheet;
                s.work_load(slug, sheet);
            }
        });
    });
    ui.on_select_cell(|coord, raw| {
        with_state(|s| s.work_select_cell(&coord, &raw));
    });
    ui.on_edit_cell(|coord, raw| {
        with_state(|s| {
            let slug = s.work.open_slug.clone();
            if !slug.is_empty() {
                s.work_apply_cell_edit(
                    slug,
                    s.work.active_sheet,
                    coord.to_string(),
                    raw.to_string(),
                );
            }
        });
    });
    ui.on_present_deck(|slug| {
        with_state(|s| {
            let slug = slug.to_string();
            if !slug.is_empty() && s.work.open_slug != slug {
                s.work_open(slug);
            }
            s.work_ui().set_presenting(true);
        });
    });
    ui.on_stop_present(|| with_state(|s| s.work_ui().set_presenting(false)));
    ui.on_add_comment(|path, comment| {
        crate::chat::add_work_comment(&path, &comment);
    });
    ui.on_open_folder(|slug| {
        let Some(root) = current_root() else {
            return;
        };
        let dir = root.join(harness_work::WORK_DIR).join(slug.as_str());
        crate::chat::open_path(&dir.to_string_lossy());
    });
    ui.on_export_format(|slug, fmt| {
        let Some(root) = current_root() else {
            return;
        };
        let slug = slug.to_string();
        let fmt = fmt.to_string();
        let _ = std::thread::Builder::new()
            .name("work-export".into())
            .spawn(move || {
                let res = actions::export_item(&root, &slug, &fmt);
                let _ = slint::invoke_from_event_loop(move || match res {
                    Ok(paths) => {
                        if let Some(first) = paths.first() {
                            crate::chat::toast_text(&format!("Exported {}", first.display()));
                            crate::chat::open_path(&first.to_string_lossy());
                        }
                    }
                    Err(e) => crate::chat::toast_text(&format!("Export failed: {e}")),
                });
            });
    });
}
