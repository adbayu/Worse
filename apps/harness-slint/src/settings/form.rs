//! Config entry form and raw editor: create, edit, save and close one entry.

use super::entry::{body_from, default_body, fields_of, transport_id, HOOK_EVENTS};
use super::{
    model, sv, with_state, After, AgentKind, AppWindow, ComponentHandle, ConfigUi, Edit, FormField,
    PathBuf, PlanOp, Req, SelectOption, SharedString, State,
};

impl State {
    pub(super) fn show_form(&self) {
        let cfg = self.window.global::<ConfigUi>();
        let Some(edit) = &self.edit else {
            cfg.set_form(model(Vec::new()));
            return;
        };
        let transports: Vec<SelectOption> = self
            .support
            .as_ref()
            .map(|s| s.transports.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|t| SelectOption {
                id: sv(transport_id(t)),
                label: sv(transport_id(t)),
            })
            .collect();
        let form: Vec<FormField> = edit
            .fields
            .iter()
            .map(|(key, value)| {
                let (kind, options) = match key.as_str() {
                    "transport" => ("select", transports.clone()),
                    "event" => (
                        "select",
                        HOOK_EVENTS
                            .iter()
                            .map(|e| SelectOption {
                                id: sv(e),
                                label: sv(e),
                            })
                            .collect(),
                    ),
                    "args" | "env" | "headers" | "body" | "systemPrompt" => {
                        ("multiline", Vec::new())
                    }
                    "command" | "url" | "matcher" => ("mono", Vec::new()),
                    "async" => ("switch", Vec::new()),
                    _ => ("text", Vec::new()),
                };
                FormField {
                    key: sv(key),
                    value: sv(value),
                    kind: sv(kind),
                    options: model(options),
                    enabled: true,
                }
            })
            .collect();
        cfg.set_form(model(form));
    }

    pub(super) fn open_editor(&mut self, name: Option<&str>) {
        let (Some(concept), target) = self.target() else {
            return;
        };
        // Only the Harness overlay carries the per-agent filter (`agents`).
        let is_overlay = target.agent == AgentKind::Harness;
        let cfg = self.window.global::<ConfigUi>();
        let entry = name.and_then(|n| self.entries.iter().find(|e| e.name == n).cloned());
        let base = entry
            .as_ref()
            .map_or_else(|| default_body(concept), |e| e.body.clone());
        self.edit = Some(Edit {
            previous: entry.as_ref().map(|e| e.name.clone()),
            fields: fields_of(&base, is_overlay),
            base: Some(base),
        });
        cfg.set_editing(true);
        cfg.set_is_new(entry.is_none());
        cfg.set_raw_mode(false);
        cfg.set_edit_name(sv(name.unwrap_or_default()));
        cfg.set_error(SharedString::new());
        cfg.set_raw(SharedString::new());
        let path = entry.map(|e| e.source_path);
        cfg.set_raw_path(sv(path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()));
        if let Some(path) = path {
            self.seq += 1;
            self.send(Req::ReadRaw {
                seq: self.seq,
                path,
            });
        }
        self.show_form();
    }

    pub(super) fn raw_loaded(&self, seq: u64, text: Result<String, String>) {
        if seq != self.seq {
            return;
        }
        let cfg = self.window.global::<ConfigUi>();
        match text {
            Ok(text) => cfg.set_raw(sv(text)),
            Err(error) => cfg.set_error(sv(error)),
        }
    }

    pub(super) fn close_editor(&mut self) {
        self.edit = None;
        let cfg = self.window.global::<ConfigUi>();
        cfg.set_editing(false);
        cfg.set_raw_mode(false);
        cfg.set_error(SharedString::new());
        cfg.set_form(model(Vec::new()));
    }

    pub(super) fn save_editor(&mut self) {
        let (_, target) = self.target();
        // Only the Harness overlay carries the per-agent filter (`agents`).
        let is_overlay = target.agent == AgentKind::Harness;
        let cfg = self.window.global::<ConfigUi>();
        let Some(edit) = &self.edit else {
            return;
        };
        let op = if cfg.get_raw_mode() && !cfg.get_raw_path().is_empty() {
            PlanOp::Raw {
                path: PathBuf::from(cfg.get_raw_path().as_str()),
                text: cfg.get_raw().to_string(),
            }
        } else {
            let Some(base) = edit.base.clone() else {
                return;
            };
            PlanOp::Upsert {
                target,
                body: Box::new(body_from(base, &edit.fields, is_overlay)),
                previous: edit.previous.clone(),
            }
        };
        self.after = After::Config;
        self.send(Req::Plan { title: "save", op });
    }
}

pub(super) fn wire(window: &AppWindow) {
    let cfg = window.global::<ConfigUi>();
    cfg.on_set_field(|key, value| {
        with_state(|s| {
            if let Some(edit) = s.edit.as_mut() {
                if let Some(field) = edit.fields.iter_mut().find(|(k, _)| *k == key.as_str()) {
                    field.1 = value.to_string();
                }
            }
        });
    });
    cfg.on_save(|| with_state(State::save_editor));
    cfg.on_close_editor(|| with_state(State::close_editor));
    cfg.on_open_folder(|_| {});
}
