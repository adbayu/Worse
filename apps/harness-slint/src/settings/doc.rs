//! Instructions and memory pages: one document section at a time, edited and searched in place.

use super::entry::{agent_kind, scope_of};
use super::{
    docview, model, sv, with_state, After, AppWindow, ComponentHandle, ConfigTarget, Doc, DocKind,
    DocUi, PlanOp, Req, SharedString, State,
};

impl State {
    // ---- instructions / memory ---------------------------------------------------------------------------

    pub(super) fn load_doc(&mut self) {
        let doc = self.window.global::<DocUi>();
        let scope = scope_of(&doc.get_scope());
        let kind = if doc.get_kind() == "memory" {
            DocKind::Memory
        } else {
            DocKind::Instructions(agent_kind(&doc.get_agent()))
        };
        self.seq += 1;
        doc.set_error(SharedString::new());
        doc.set_saved(SharedString::new());
        self.send(Req::Doc {
            seq: self.seq,
            kind,
            scope,
        });
    }

    pub(super) fn doc_loaded(&mut self, seq: u64, loaded: Result<Doc, String>) {
        if seq != self.seq {
            return;
        }
        let ui = self.window.global::<DocUi>();
        match loaded {
            Ok(doc) => {
                ui.set_supported(doc.supported);
                ui.set_path(sv(doc.path.display().to_string()));
                ui.set_exists(doc.exists);
                ui.set_text(sv(&doc.text));
                ui.set_bytes(i32::try_from(doc.text.len()).unwrap_or(i32::MAX));
                ui.set_dirty(false);
                ui.set_error(sv(&doc.reason));
                self.doc_original = doc.text;
                self.doc_path = doc.path;
                ui.set_selected(-1);
                self.search_doc();
            }
            Err(error) => {
                ui.set_supported(true);
                ui.set_error(sv(error));
            }
        }
    }

    pub(super) fn search_doc(&self) {
        let ui = self.window.global::<DocUi>();
        let query = ui.get_query().to_lowercase();
        let matches: Vec<SharedString> = if query.is_empty() {
            Vec::new()
        } else {
            ui.get_text()
                .lines()
                .filter(|l| l.to_lowercase().contains(&query))
                .map(sv)
                .collect()
        };
        ui.set_matches(model(matches));
        docview::refresh(&ui, &self.doc_original, &self.doc_path);
    }

    pub(super) fn save_doc(&mut self) {
        let ui = self.window.global::<DocUi>();
        let text = ui.get_text().to_string();
        let scope = scope_of(&ui.get_scope());
        let (title, op) = if ui.get_kind() == "memory" {
            ("memory", PlanOp::Memory { scope, text })
        } else {
            (
                "instructions",
                PlanOp::Instructions {
                    target: ConfigTarget::new(agent_kind(&ui.get_agent()), scope),
                    path: self.doc_path.clone(),
                    text,
                },
            )
        };
        self.after = After::Doc;
        self.send(Req::Plan { title, op });
    }
}

pub(super) fn wire(window: &AppWindow) {
    let doc = window.global::<DocUi>();
    doc.on_select_agent(|agent| {
        with_state(|s| {
            s.window.global::<DocUi>().set_agent(agent);
            s.load_doc();
        });
    });
    doc.on_select_scope(|scope| {
        with_state(|s| {
            s.window.global::<DocUi>().set_scope(scope);
            s.load_doc();
        });
    });
    doc.on_edited(|text| {
        with_state(|s| {
            let ui = s.window.global::<DocUi>();
            ui.set_dirty(text.as_str() != s.doc_original);
            ui.set_bytes(i32::try_from(text.len()).unwrap_or(i32::MAX));
            ui.set_saved(SharedString::new());
            s.search_doc();
        });
    });
    doc.on_save(|| with_state(State::save_doc));
    doc.on_revert(|| {
        with_state(|s| {
            let ui = s.window.global::<DocUi>();
            ui.set_text(sv(&s.doc_original));
            ui.set_dirty(false);
            ui.set_bytes(i32::try_from(s.doc_original.len()).unwrap_or(i32::MAX));
            s.search_doc();
        });
    });
    doc.on_refresh(|| with_state(State::load_doc));
    doc.on_search(|_| with_state(|s| s.search_doc()));
    // Memory entries (design S8): edit one section of MEMORY.md at a time.
    doc.on_select_entry(|index| {
        with_state(|s| {
            let ui = s.window.global::<DocUi>();
            ui.set_selected(index);
            ui.set_entry_text(sv(docview::entry_text(&ui.get_text(), index)));
        });
    });
    doc.on_entry_edited(|section| {
        with_state(|s| {
            let text = {
                let ui = s.window.global::<DocUi>();
                docview::replace_entry(&ui.get_text(), ui.get_selected(), Some(&section))
            };
            set_doc_text(s, text);
        });
    });
    doc.on_new_entry(|| {
        with_state(|s| {
            let text = {
                let ui = s.window.global::<DocUi>();
                let (text, index) = docview::append_entry(&ui.get_text());
                ui.set_selected(index);
                ui.set_entry_text(sv(docview::entry_text(&text, index)));
                text
            };
            set_doc_text(s, text);
        });
    });
    doc.on_delete_entry(|| {
        with_state(|s| {
            let text = {
                let ui = s.window.global::<DocUi>();
                let text = docview::replace_entry(&ui.get_text(), ui.get_selected(), None);
                ui.set_selected(-1);
                text
            };
            set_doc_text(s, text);
        });
    });
}

/// Replaces the whole document text, refreshing the dirty flag, byte count and search matches.
pub(super) fn set_doc_text(s: &mut State, text: String) {
    {
        let ui = s.window.global::<DocUi>();
        ui.set_dirty(text != s.doc_original);
        ui.set_bytes(i32::try_from(text.len()).unwrap_or(i32::MAX));
        ui.set_saved(SharedString::new());
        ui.set_text(sv(text));
    }
    s.search_doc();
}
