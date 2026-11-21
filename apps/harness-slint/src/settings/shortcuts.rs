//! Shortcuts page: rows, key recording, conflict stealing, and the shared command dispatcher.

use super::{
    keymap, model, sv, with_state, AppWindow, Arc, BTreeMap, ComponentHandle, Keymap, Labels,
    Press, Req, SharedString, ShortcutRow, ShortcutsUi, State, Theme, Value, Verdict, KEYMAP,
};

impl State {
    // ---- shortcuts ---------------------------------------------------------------------------------------

    pub(super) fn refresh_shortcuts(&self) {
        let ui = self.window.global::<ShortcutsUi>();
        let labels = self.window.global::<Labels>();
        let query = ui.get_query().to_lowercase();
        let key = ui.get_key_query().to_string();
        let key_search = ui.get_key_search();
        let rows = keymap(|k| {
            k.rows
                .iter()
                .filter(|(id, _, bindings, _)| {
                    if key_search && !key.is_empty() {
                        return bindings.iter().any(|b| keymap::same(b, &key));
                    }
                    query.is_empty()
                        || labels
                            .invoke_command_label(sv(id))
                            .to_lowercase()
                            .contains(&query)
                        || id.to_lowercase().contains(&query)
                        || bindings
                            .iter()
                            .any(|b| keymap::display(b).to_lowercase().contains(&query))
                })
                .map(|(id, scope, bindings, custom)| ShortcutRow {
                    id: sv(id),
                    scope: sv(if *scope == keymap::Scope::Composer {
                        "composer"
                    } else {
                        "global"
                    }),
                    bindings: model(bindings.iter().map(sv).collect()),
                    custom: *custom,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
        // Settings S6 groups rows under caps headings: global commands first, composer keys last (stable).
        let mut rows = rows;
        rows.sort_by_key(|r| r.scope == "composer");
        ui.set_rows(model(rows));
    }

    pub(super) fn save_shortcuts(&mut self, edit: impl FnOnce(&mut BTreeMap<String, Vec<String>>)) {
        let mut map = self.settings.shortcuts.clone();
        edit(&mut map);
        // 乐观更新本地键位表，store 的 watch 稍后给出权威值。
        KEYMAP.with(|cell| *cell.borrow_mut() = Some(Keymap::new(&map)));
        let mut next = (*self.settings).clone();
        next.shortcuts.clone_from(&map);
        self.settings = Arc::new(next);
        self.send(Req::Shortcuts(map));
        self.refresh_shortcuts();
    }

    pub(super) fn stop_recording(&self) {
        let ui = self.window.global::<ShortcutsUi>();
        ui.set_recording(SharedString::new());
        ui.set_recorded(SharedString::new());
        ui.set_error(SharedString::new());
        ui.set_conflict(SharedString::new());
    }

    #[allow(clippy::fn_params_excessive_bools)] // Slint KeyEvent modifiers, one flag each.
    pub(super) fn key(
        &mut self,
        text: &str,
        ctrl: bool,
        alt: bool,
        shift: bool,
        meta: bool,
    ) -> bool {
        let ui = self.window.global::<ShortcutsUi>();
        let id = ui.get_recording().to_string();
        let press = keymap::press(text, ctrl, alt, shift, meta);
        if id.is_empty() {
            if !ui.get_key_search() {
                return false;
            }
            match press {
                Press::Escape => {
                    ui.set_key_search(false);
                    ui.set_key_query(SharedString::new());
                }
                Press::Binding(binding) => ui.set_key_query(sv(binding)),
                _ => return true,
            }
            self.refresh_shortcuts();
            return true;
        }
        match press {
            Press::Escape => self.stop_recording(),
            Press::Backspace => {
                self.save_shortcuts(|m| {
                    m.remove(&id);
                });
                self.stop_recording();
            }
            Press::ModifierOnly => {}
            Press::Invalid => {
                ui.set_error(sv("invalidKey"));
                ui.set_conflict(SharedString::new());
            }
            Press::Binding(binding) => {
                let verdict = keymap(|k| keymap::check(k, &id, &binding)).unwrap_or(Verdict::Ok);
                ui.set_recorded(sv(&binding));
                match verdict {
                    Verdict::Ok => {
                        self.save_shortcuts(|m| {
                            m.insert(id.clone(), vec![binding]);
                        });
                        self.stop_recording();
                    }
                    Verdict::Error(code) => {
                        ui.set_error(sv(code));
                        ui.set_conflict(SharedString::new());
                    }
                    Verdict::Occupied(owner) => {
                        ui.set_error(sv("occupied"));
                        ui.set_conflict(sv(owner));
                    }
                }
            }
        }
        true
    }

    pub(super) fn steal(&mut self) {
        let ui = self.window.global::<ShortcutsUi>();
        let id = ui.get_recording().to_string();
        let owner = ui.get_conflict().to_string();
        let binding = ui.get_recorded().to_string();
        if id.is_empty() || owner.is_empty() || binding.is_empty() {
            return;
        }
        let owner_bindings: Vec<String> = keymap(|k| k.bindings(&owner))
            .unwrap_or_default()
            .into_iter()
            .filter(|b| !keymap::same(b, &binding))
            .collect();
        self.save_shortcuts(|m| {
            m.insert(owner, owner_bindings);
            m.insert(id, vec![binding]);
        });
        self.stop_recording();
    }

    pub(super) fn run_command(&self, id: &str) {
        let zoom = i64::from(self.settings.appearance.zoom_percent);
        match id {
            "openSettings" => self.window.set_settings_open(true),
            "switchTheme" => {
                let dark = self.window.global::<Theme>().get_dark();
                self.patch("appearance.theme", Value::from(if dark { 1 } else { 2 }));
            }
            "zoomIn" => self.patch("appearance.zoomPercent", Value::from((zoom + 10).min(150))),
            "zoomOut" => self.patch("appearance.zoomPercent", Value::from((zoom - 10).max(80))),
            "resetZoom" => self.patch("appearance.zoomPercent", Value::from(100)),
            "toggleSidePane" => self.window.set_files_open(!self.window.get_files_open()),
            "toggleAutoCompact" => self.patch(
                "general.autoCompact",
                Value::from(!self.settings.general.auto_compact),
            ),
            "newTask" => {
                self.window.set_settings_open(false);
                self.window.invoke_new_session();
            }
            // 其余命令（命令中心、查找、导航、导出…）由 src/chat 处理；没有对应功能的命令什么也不做。
            other => {
                let other = other.to_string();
                // 在当前借用结束后再交给 chat，避免 chat 回调设置页时重入。
                slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                    crate::chat::command(&other);
                });
            }
        }
    }
}

/// Display text of the first binding of a keymap command ("" when unbound or unknown).
pub fn shortcut_label(id: &str) -> String {
    keymap(|k| k.bindings(id).first().map(|b| keymap::display(b)))
        .flatten()
        .unwrap_or_default()
}

/// Runs a global command (command center, slash commands) through the same dispatcher as the shortcuts.
pub fn run_command(id: &str) {
    with_state(|s| s.run_command(id));
}

pub(super) fn wire(window: &AppWindow) {
    let keys = window.global::<ShortcutsUi>();
    keys.on_display(|b| sv(keymap::display(&b)));
    keys.on_display_keys(|b| {
        // "Ctrl+Shift+O" -> ["Ctrl", "Shift", "O"]; a literal "+" key ("Ctrl++") stays one cap.
        let text = keymap::display(&b);
        let mut caps: Vec<SharedString> = Vec::new();
        let mut rest = text.as_str();
        while !rest.is_empty() {
            match rest.find('+') {
                Some(0) => {
                    caps.push(sv("+"));
                    rest = rest[1..].strip_prefix('+').unwrap_or(&rest[1..]);
                }
                Some(i) => {
                    caps.push(sv(&rest[..i]));
                    rest = &rest[i + 1..];
                }
                None => {
                    caps.push(sv(rest));
                    rest = "";
                }
            }
        }
        model(caps)
    });
    keys.on_match(|text, ctrl, alt, shift, meta| {
        match keymap::press(&text, ctrl, alt, shift, meta) {
            Press::Binding(b) => keymap(|k| k.command_for(&b).map(sv))
                .flatten()
                .unwrap_or_default(),
            _ => SharedString::new(),
        }
    });
    keys.on_key(|text, ctrl, alt, shift, meta| {
        let mut consumed = false;
        with_state(|s| consumed = s.key(&text, ctrl, alt, shift, meta));
        consumed
    });
    keys.on_start_record(|id| {
        with_state(|s| {
            s.stop_recording();
            let ui = s.window.global::<ShortcutsUi>();
            ui.set_key_search(false);
            ui.set_recording(id);
        });
    });
    keys.on_cancel_record(|| with_state(|s| s.stop_recording()));
    keys.on_steal(|| with_state(State::steal));
    keys.on_clear(|id| {
        with_state(|s| {
            s.save_shortcuts(|m| {
                m.insert(id.to_string(), Vec::new());
            });
        });
    });
    keys.on_reset_all(|| with_state(|s| s.save_shortcuts(BTreeMap::clear)));
    keys.on_search(|_| with_state(|s| s.refresh_shortcuts()));
    keys.on_start_key_search(|| {
        with_state(|s| {
            s.stop_recording();
            let ui = s.window.global::<ShortcutsUi>();
            ui.set_key_search(true);
            ui.set_key_query(SharedString::new());
            s.refresh_shortcuts();
        });
    });
    keys.on_clear_key_search(|| {
        with_state(|s| {
            let ui = s.window.global::<ShortcutsUi>();
            ui.set_key_search(false);
            ui.set_key_query(SharedString::new());
            s.refresh_shortcuts();
        });
    });
}
