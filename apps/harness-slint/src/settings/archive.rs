//! Archive page: paged list of archived sessions.

use super::util::{date_of, folder_name};
use super::{
    api, model, sv, with_state, AppWindow, ArchiveRow, ArchiveUi, ComponentHandle, Req, State,
};

impl State {
    // ---- archive -----------------------------------------------------------------------------------------

    pub(super) fn refresh_archive(&mut self, offset: u32) {
        self.list_seq += 1;
        let query = self.window.global::<ArchiveUi>().get_query().to_string();
        self.send(Req::Archived {
            seq: self.list_seq,
            query,
            offset,
        });
    }

    pub(super) fn archive_listed(
        &mut self,
        seq: u64,
        offset: u32,
        result: Result<api::ArchivePage, String>,
    ) {
        if seq != self.list_seq {
            return;
        }
        let ui = self.window.global::<ArchiveUi>();
        match result {
            Ok(page) => {
                if offset == 0 {
                    self.archive.clear();
                }
                let names = crate::bridge::agent_names();
                self.archive.extend(page.rows.into_iter().map(|a| {
                    ArchiveRow {
                        id: sv(&a.summary.id.0),
                        title: sv(&a.summary.title),
                        project: sv(folder_name(&a.summary.workspace_path)),
                        updated: sv(date_of(a.archived_at)),
                        agent: sv(names
                            .iter()
                            .find(|(id, _)| *id == a.summary.active_agent.to_string())
                            .map_or_else(
                                || a.summary.active_agent.to_string(),
                                |(_, n)| n.clone(),
                            )),
                    }
                }));
                ui.set_available(true);
                ui.set_has_more(page.has_more);
                ui.set_rows(model(self.archive.clone()));
            }
            Err(error) => {
                ui.set_available(false);
                ui.set_has_more(false);
                self.archive.clear();
                ui.set_rows(model(Vec::new()));
                self.status(&error, true);
            }
        }
    }

    pub(super) fn archive_changed(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.status(&error, true);
        }
        self.refresh_archive(0);
    }
}

pub(super) fn wire(window: &AppWindow) {
    let archive = window.global::<ArchiveUi>();
    archive.on_search(|_| with_state(|s| s.refresh_archive(0)));
    archive.on_unarchive(|id| with_state(|s| s.send(Req::Unarchive(id.to_string()))));
    archive.on_delete(|id| with_state(|s| s.send(Req::DeleteArchived(id.to_string()))));
    archive.on_more(|| {
        with_state(|s| {
            let offset = u32::try_from(s.archive.len()).unwrap_or(u32::MAX);
            s.refresh_archive(offset);
        });
    });
}
