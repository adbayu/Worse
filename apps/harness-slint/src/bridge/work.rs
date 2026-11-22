//! Work edition UI bridge (spec `docs/specs/harness/work-edition.md`): connects `WorkUi` (side
//! panel + present mode), `WorkLibraryUi` (library in the side panel Work tab), and chat `WorkCard`s to
//! `harness-work` on background worker threads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use harness_work::ItemSummary;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{with_state, UiState};
use crate::ui::{SheetRowView, SheetTab, SlideThumb, WorkItemCard, WorkLibraryUi, WorkUi};

mod actions;
mod grid;
mod load;
mod wire;

use load::{kind_str, load_item, scan_items, summary_detail, LoadedWork};
pub(super) use wire::wire;

#[derive(Default)]
pub(super) struct WorkState {
    library: Vec<ItemSummary>,
    pub(super) filter_kind: String,
    pub(super) search_query: String,
    pub(super) open_slug: String,
    pub(super) active_sheet: usize,
    /// Decoded preview images per slide/page so switching slides is instant on the UI thread.
    previews: Vec<Option<slint::Image>>,
    notes: Vec<String>,
    /// Cell notes/error tooltips and validation dropdown lists of the open sheet.
    sheet_notes: HashMap<String, String>,
    sheet_validations: Vec<(String, Vec<String>)>,
    /// Queued cell edit while an agent turn is running (single-writer rule, W6.3).
    queued_cell: Option<(String, usize, String, String)>,
}

pub(super) fn sv(text: &str) -> SharedString {
    SharedString::from(text)
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

impl UiState {
    pub(super) fn work_ui(&self) -> WorkUi<'_> {
        self.window.global::<WorkUi>()
    }

    pub(super) fn work_lib_ui(&self) -> WorkLibraryUi<'_> {
        self.window.global::<WorkLibraryUi>()
    }

    /// Workspace of the open session, falling back to the app workspace.
    pub(super) fn work_root(&self) -> PathBuf {
        self.active
            .as_ref()
            .and_then(|id| self.summaries.iter().find(|s| &s.id == id))
            .map(|s| s.workspace_path.clone())
            .filter(|p| !p.trim().is_empty())
            .map_or_else(|| self.workspace.clone(), PathBuf::from)
    }

    pub(super) fn work_detect_tool(&self, title: &str) -> Option<(String, String, String, String)> {
        let root = self.work_root();
        actions::detect_work_item(title, &root)
    }

    /// 修复：侧栏打开的 Work 项目没有随消息发送，agent 看不到它（只会去找本机 Excel）。
    /// The Work item open in the side panel goes along with every message as context.
    pub(super) fn add_open_work(&self, context: &mut Vec<harness_core_api::ContextItem>) {
        if self.work.open_slug.is_empty()
            || !self.window.get_files_open()
            || self.window.get_side_tab() != "work"
        {
            return;
        }
        let item = harness_core_api::ContextItem::WorkItem {
            path: format!("work/{}", self.work.open_slug),
        };
        if !context.contains(&item) {
            context.push(item);
        }
    }

    pub(super) fn work_turn_finished(&mut self) {
        if let Some((slug, sheet, coord, raw)) = self.work.queued_cell.take() {
            self.work_apply_cell_edit(slug, sheet, coord, raw);
        } else if !self.work.open_slug.is_empty() && self.window.get_files_open() {
            let slug = self.work.open_slug.clone();
            self.work_load(slug, self.work.active_sheet);
        }
        if self.work.open_slug.is_empty()
            && self.window.get_files_open()
            && self.window.get_side_tab() == "work"
        {
            self.work_refresh_library();
        }
    }

    /// Side panel Work tab without an open item: the library card grid.
    pub(super) fn work_show_library(&mut self) {
        self.work.open_slug.clear();
        self.work.active_sheet = 0;
        self.work.queued_cell = None;
        let ui = self.work_ui();
        ui.set_item_slug(SharedString::new());
        ui.set_item_title(SharedString::new());
        ui.set_status_text(SharedString::new());
        ui.set_warning_text(SharedString::new());
        ui.set_has_preview(false);
        self.window.set_files_open(true);
        self.window.set_side_tab(sv("work"));
        self.work_refresh_library();
    }

    pub(super) fn work_refresh_library(&self) {
        let root = self.work_root();
        self.work_lib_ui().set_loading(true);
        let _ = std::thread::Builder::new()
            .name("work-scan".into())
            .spawn(move || {
                let items = scan_items(&root);
                let _ = slint::invoke_from_event_loop(move || {
                    with_state(|s| {
                        s.work.library = items;
                        s.work_lib_ui().set_loading(false);
                        s.work_apply_library_filter();
                    });
                });
            });
    }

    pub(super) fn work_apply_library_filter(&self) {
        let kind = if self.work.filter_kind.is_empty() {
            "all"
        } else {
            self.work.filter_kind.as_str()
        };
        let q = self.work.search_query.trim().to_ascii_lowercase();
        let rows: Vec<WorkItemCard> = self
            .work
            .library
            .iter()
            .filter(|item| kind == "all" || kind_str(item.kind) == kind)
            .filter(|item| {
                q.is_empty()
                    || item.title.to_ascii_lowercase().contains(&q)
                    || item.slug.to_ascii_lowercase().contains(&q)
            })
            .map(|item| WorkItemCard {
                slug: sv(&item.slug),
                title: sv(&item.title),
                kind: sv(kind_str(item.kind)),
                updated: SharedString::new(),
                summary: sv(&summary_detail(item)),
            })
            .collect();
        self.work_lib_ui()
            .set_items(ModelRc::from(Rc::new(VecModel::from(rows))));
    }

    pub(super) fn work_open(&mut self, slug: String) {
        self.work_lib_ui().set_open(false);
        self.window.set_files_open(true);
        self.window.set_side_tab(sv("work"));
        self.work.open_slug.clone_from(&slug);
        self.work.active_sheet = 0;
        self.work_load(slug, 0);
    }

    pub(super) fn work_load(&self, slug: String, sheet_idx: usize) {
        let ui = self.work_ui();
        ui.set_open(true);
        ui.set_item_slug(sv(&slug));
        ui.set_loading(true);
        ui.set_warning_text(SharedString::new());
        let root = self.work_root();
        let _ = std::thread::Builder::new()
            .name("work-load".into())
            .spawn(move || {
                let res = load_item(&root, &slug, sheet_idx);
                let _ = slint::invoke_from_event_loop(move || {
                    with_state(|s| s.apply_loaded_work(res));
                });
            });
    }

    fn apply_loaded_work(&mut self, result: Result<LoadedWork, String>) {
        self.work_ui().set_loading(false);
        let loaded = match result {
            Ok(l) => l,
            Err(err) => {
                let ui = self.work_ui();
                ui.set_has_preview(false);
                ui.set_warning_text(sv(&err));
                return;
            }
        };
        self.work.open_slug.clone_from(&loaded.slug);
        self.work.active_sheet = loaded.active_sheet;
        {
            let ui = self.work_ui();
            ui.set_item_slug(sv(&loaded.slug));
            ui.set_item_title(sv(&loaded.title));
            ui.set_item_kind(sv(&loaded.kind));
            ui.set_status_text(sv(&loaded.status));
            ui.set_warning_text(sv(&loaded.warning));
        }

        if loaded.kind == "sheet" {
            self.apply_loaded_sheet(loaded);
            return;
        }
        self.apply_loaded_pages(loaded);
    }

    fn apply_loaded_sheet(&mut self, loaded: LoadedWork) {
        self.work.previews.clear();
        self.work.notes.clear();
        self.work.sheet_notes = loaded.sheet_notes;
        self.work.sheet_validations = loaded.sheet_validations;
        let ui = self.work_ui();
        ui.set_has_preview(false);
        ui.set_slides(ModelRc::default());
        ui.set_notes(SharedString::new());
        ui.set_cell_note(SharedString::new());
        ui.set_validation_options(ModelRc::default());
        let tabs: Vec<SheetTab> = loaded
            .sheet_names
            .iter()
            .enumerate()
            .filter(|(i, _)| !loaded.sheet_hidden.get(*i).copied().unwrap_or(false))
            .map(|(i, n)| SheetTab {
                index: to_i32(i),
                name: sv(n),
            })
            .collect();
        ui.set_sheets(ModelRc::from(Rc::new(VecModel::from(tabs))));
        ui.set_active_sheet(to_i32(loaded.active_sheet));
        let headers: Vec<SharedString> = loaded.col_headers.iter().map(|c| sv(&c.0)).collect();
        let widths: Vec<f32> = loaded.col_headers.iter().map(|c| c.1).collect();
        let grid_height: f32 = loaded.sheet_rows.iter().map(|r| r.height).sum();
        ui.set_grid_width(widths.iter().sum());
        ui.set_grid_height(grid_height);
        ui.set_col_headers(ModelRc::from(Rc::new(VecModel::from(headers))));
        ui.set_col_widths(ModelRc::from(Rc::new(VecModel::from(widths))));
        let rows: Vec<SheetRowView> = loaded
            .sheet_rows
            .into_iter()
            .map(|r| SheetRowView {
                row_num: r.row_num,
                height: r.height,
                cells: ModelRc::from(Rc::new(VecModel::from(
                    r.cells.iter().map(grid::to_view).collect::<Vec<_>>(),
                ))),
            })
            .collect();
        ui.set_sheet_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        let sheet_name = loaded
            .sheet_names
            .get(loaded.active_sheet)
            .map_or("Sheet1", String::as_str);
        ui.set_selected_path(sv(&format!(
            "work/{}/book.json#{sheet_name}!A1",
            loaded.slug
        )));
    }

    pub(super) fn work_select_cell(&mut self, coord: &str, raw: &str) {
        let path = format!("work/{}/book.json#{coord}", self.work.open_slug);
        let note = self
            .work
            .sheet_notes
            .get(coord)
            .cloned()
            .unwrap_or_default();
        let opts: Vec<SharedString> = grid::validation_for(coord, &self.work.sheet_validations)
            .into_iter()
            .map(|s| sv(&s))
            .collect();
        let ui = self.work_ui();
        ui.set_selected_coord(sv(coord));
        ui.set_formula_text(sv(raw));
        ui.set_selected_path(sv(&path));
        ui.set_cell_note(sv(&note));
        ui.set_validation_options(ModelRc::from(Rc::new(VecModel::from(opts))));
    }

    fn apply_loaded_pages(&mut self, loaded: LoadedWork) {
        let mut previews = Vec::with_capacity(loaded.slides.len());
        let mut notes = Vec::with_capacity(loaded.slides.len());
        let mut thumbs = Vec::with_capacity(loaded.slides.len());
        for s in loaded.slides {
            previews.push(s.preview.map(slint::Image::from_rgba8));
            notes.push(s.notes);
            let (image, has_image) = match s.thumb {
                Some(buf) => (slint::Image::from_rgba8(buf), true),
                None => (slint::Image::default(), false),
            };
            thumbs.push(SlideThumb {
                index: to_i32(s.index),
                title: sv(&s.title),
                image,
                has_image,
            });
        }
        self.work.previews = previews;
        self.work.notes = notes;
        let current = {
            let ui = self.work_ui();
            ui.set_item_count(to_i32(thumbs.len()));
            ui.set_slides(ModelRc::from(Rc::new(VecModel::from(thumbs))));
            usize::try_from(ui.get_active_index()).unwrap_or(0)
        };
        let idx = current.min(self.work.previews.len().saturating_sub(1));
        self.work_show_page(idx);
    }

    pub(super) fn work_show_page(&self, idx: usize) {
        let ui = self.work_ui();
        ui.set_active_index(to_i32(idx));
        if let Some(Some(img)) = self.work.previews.get(idx) {
            ui.set_preview_image(img.clone());
            ui.set_has_preview(true);
        } else {
            ui.set_has_preview(false);
        }
        let note = self.work.notes.get(idx).map_or("", String::as_str);
        ui.set_notes(sv(note));
        let kind = ui.get_item_kind().to_string();
        let slug = self.work.open_slug.as_str();
        let path = match kind.as_str() {
            "deck" => format!("work/{slug}/deck.json#/slides/{idx}"),
            "doc" => format!("work/{slug}/doc.md#page-{}", idx + 1),
            _ => format!("work/{slug}/chart.json"),
        };
        ui.set_selected_path(sv(&path));
    }

    pub(super) fn work_apply_cell_edit(
        &mut self,
        slug: String,
        sheet: usize,
        coord: String,
        raw: String,
    ) {
        if self.window.get_running() {
            self.work.queued_cell = Some((slug, sheet, coord, raw));
            crate::chat::toast_text("Agent is editing — cell change queued");
            return;
        }
        let root = self.work_root();
        let _ = std::thread::Builder::new()
            .name("work-edit".into())
            .spawn(move || {
                let res = actions::edit_sheet_cell(&root, &slug, sheet, &coord, &raw);
                let _ = slint::invoke_from_event_loop(move || match res {
                    Ok(()) => with_state(|s| s.work_load(slug, sheet)),
                    Err(e) => crate::chat::toast_text(&format!("Edit failed: {e}")),
                });
            });
    }
}
