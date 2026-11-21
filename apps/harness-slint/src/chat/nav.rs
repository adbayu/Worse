use super::{
    export, now_ms, sv, ChatState, ChatUi, ComponentHandle, Duration, Effect, FileReq, SharedString,
};

impl ChatState {
    // ---- navigation ------------------------------------------------------------------------------------

    pub(super) fn refresh_nav(&self) {
        let ui = self.ui();
        let index = self
            .active
            .as_ref()
            .and_then(|a| self.sessions.iter().position(|(id, _)| id == a));
        ui.set_can_back(self.nav_pos > 0);
        ui.set_can_forward(self.nav_pos + 1 < self.nav.len());
        ui.set_can_prev(match index {
            Some(i) => i > 0,
            None => !self.sessions.is_empty(),
        });
        ui.set_can_next(match index {
            Some(i) => i + 1 < self.sessions.len(),
            None => !self.sessions.is_empty(),
        });
        ui.set_has_session(self.active.is_some());
    }

    pub(super) fn session_opened(&mut self, id: &str) {
        if self.nav_jump.as_deref() == Some(id) {
            self.nav_jump = None;
        } else if self.nav.get(self.nav_pos).map_or(true, |cur| cur != id) {
            if !self.nav.is_empty() {
                self.nav.truncate(self.nav_pos + 1);
            }
            self.nav.push(id.to_string());
            self.nav_pos = self.nav.len() - 1;
        }
        self.active = Some(id.to_string());
        self.close_find();
        self.refresh_nav();
    }

    pub(super) fn nav(&mut self, how: &str) -> Effect {
        let target = match how {
            "back" if self.nav_pos > 0 => {
                self.nav_pos -= 1;
                self.nav.get(self.nav_pos).cloned()
            }
            "forward" if self.nav_pos + 1 < self.nav.len() => {
                self.nav_pos += 1;
                self.nav.get(self.nav_pos).cloned()
            }
            "prev" | "next" => {
                let index = self
                    .active
                    .as_ref()
                    .and_then(|a| self.sessions.iter().position(|(id, _)| id == a));
                let next = match (index, how) {
                    (Some(i), "prev") => i.checked_sub(1),
                    (Some(i), _) => Some(i + 1),
                    (None, _) => Some(0),
                };
                return next
                    .and_then(|i| self.sessions.get(i))
                    .map_or(Effect::None, |(id, _)| Effect::OpenSession(id.clone()));
            }
            _ => None,
        };
        target.map_or(Effect::None, |id| {
            self.nav_jump = Some(id.clone());
            Effect::OpenSession(id)
        })
    }

    // ---- find ------------------------------------------------------------------------------------------

    pub(super) fn find(&mut self, query: &str) {
        self.find_query = query.to_lowercase();
        self.find_hits.clear();
        self.find_pos = 0;
        if !self.find_query.is_empty() {
            for (row, item) in crate::bridge::transcript().into_iter().enumerate() {
                let haystack = item.text.to_lowercase();
                let count = haystack.matches(self.find_query.as_str()).count();
                self.find_hits
                    .extend(std::iter::repeat((item.id, row)).take(count));
            }
        }
        self.show_find();
    }

    pub(super) fn find_step(&mut self, delta: i32) {
        let len = self.find_hits.len();
        if len == 0 {
            return;
        }
        self.find_pos = if delta < 0 {
            (self.find_pos + len - 1) % len
        } else {
            (self.find_pos + 1) % len
        };
        self.show_find();
    }

    pub(super) fn show_find(&self) {
        let ui = self.ui();
        let count = i32::try_from(self.find_hits.len()).unwrap_or(i32::MAX);
        ui.set_find_count(count);
        if let Some(&(id, _)) = self.find_hits.get(self.find_pos) {
            ui.set_find_current(i32::try_from(self.find_pos + 1).unwrap_or(i32::MAX));
            crate::bridge::reveal_row(id);
            ui.set_find_id(id);
            ui.set_find_row(crate::bridge::shown_index(id));
        } else {
            ui.set_find_current(0);
            crate::bridge::reveal_row(-1);
            ui.set_find_id(-1);
            ui.set_find_row(-1);
        }
        ui.set_find_seq(ui.get_find_seq().wrapping_add(1));
    }

    pub(super) fn close_find(&mut self) {
        self.find_query.clear();
        self.find_hits.clear();
        self.find_pos = 0;
        let ui = self.ui();
        ui.set_find_open(false);
        crate::bridge::reveal_row(-1);
        ui.set_find_id(-1);
        ui.set_find_row(-1);
        ui.set_find_count(0);
        ui.set_find_current(0);
    }

    // ---- feedback --------------------------------------------------------------------------------------

    pub(super) fn toast(&self, text: &str) {
        self.ui().set_toast(sv(text));
        let weak = self.window.as_weak();
        self.toast_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(1800),
            move || {
                if let Some(window) = weak.upgrade() {
                    window.global::<ChatUi>().set_toast(SharedString::new());
                }
            },
        );
    }

    pub(super) fn copy(&self, text: &str) {
        self.window.invoke_copy_text(sv(text));
        let label = self.ui().invoke_action_label(sv("copied"));
        self.toast(&label);
    }

    pub(super) fn export(&self, format: &str) {
        let rows = crate::bridge::transcript();
        if rows.is_empty() {
            return;
        }
        let title = self
            .active
            .as_ref()
            .and_then(|a| self.sessions.iter().find(|(id, _)| id == a))
            .map(|(_, t)| t.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Harness session".into());
        let formatter = export::formatter_for(format);
        let contents = formatter.format(&title, &rows);
        let ext = formatter.extension();
        let path = self.exports.join(format!(
            "{}-{}.{ext}",
            export::slug(&title),
            now_ms() / 1000
        ));
        let _ = self.files_tx.send(FileReq::Write { path, contents });
    }
}
