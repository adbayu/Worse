use super::{
    skill_catalog, CatalogSkill, ComponentHandle, ModelRc, SkillRow, SkillsUi, State, VecModel,
    MAX_ROWS, STATE, SUMMARY,
};

fn publish_summary(s: &State) {
    let mut out: Vec<(String, String)> = Vec::new();
    for (agent, names) in &s.installed {
        for name in names {
            match out.iter_mut().find(|(n, _)| n == name) {
                Some((_, agents)) => {
                    agents.push_str(", ");
                    agents.push_str(agent);
                }
                None => out.push((name.clone(), agent.clone())),
            }
        }
    }
    out.sort();
    if let Ok(mut slot) = SUMMARY.write() {
        *slot = out;
    }
}

pub(super) fn push() {
    let Some(window) = STATE.with(|cell| cell.borrow().as_ref().and_then(|s| s.window.upgrade()))
    else {
        return;
    };
    STATE.with(|cell| {
        let cell = cell.borrow();
        let Some(s) = cell.as_ref() else {
            return;
        };
        let ui = window.global::<SkillsUi>();
        let query = s.query.to_lowercase();
        let mut rows = Vec::new();
        let mut hidden = 0usize;
        for entry in &s.catalog {
            let hay = format!("{} {}", entry.name, entry.description).to_lowercase();
            if !query.is_empty() && !hay.contains(&query) {
                continue;
            }
            if rows.len() >= MAX_ROWS {
                hidden += 1;
                continue;
            }
            let installed = s
                .installed
                .values()
                .any(|names| names.iter().any(|n| n == &entry.name));
            rows.push(SkillRow {
                name: entry.name.clone().into(),
                description: entry.description.clone().into(),
                group: entry.group.clone().into(),
                installed,
                update: installed && stale(s, entry),
                busy: s.busy == entry.name,
                health: s
                    .health
                    .get(&entry.name)
                    .map(|(grade, _)| grade.clone())
                    .unwrap_or_default()
                    .into(),
                health_detail: s
                    .health
                    .get(&entry.name)
                    .map(|(_, detail)| detail.clone())
                    .unwrap_or_default()
                    .into(),
            });
        }
        // Installed skills that are not in the catalog (their own folder, a Harness overlay) still need a row so
        // their health badge is reachable; they have no group, so Install/Update stay hidden and Remove works.
        let known: Vec<String> = s.catalog.iter().map(|e| e.name.clone()).collect();
        let mut extra: Vec<&String> = s
            .installed
            .values()
            .flatten()
            .filter(|name| !known.iter().any(|k| k == *name))
            .collect();
        extra.sort();
        extra.dedup();
        for name in extra {
            if !query.is_empty() && !name.to_lowercase().contains(&query) {
                continue;
            }
            if rows.len() >= MAX_ROWS {
                hidden += 1;
                continue;
            }
            rows.push(local_row(s, name));
        }
        ui.set_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_hidden(i32::try_from(hidden).unwrap_or(i32::MAX));
        ui.set_loading(!s.busy.is_empty() || (s.fetching && s.catalog.is_empty()));
        ui.set_catalog_available(s.available);
        ui.set_status(s.status.clone().into());
        ui.set_status_error(s.status_error);
        ui.set_health_grade(s.health_grade.clone().into());
        ui.set_health_score(i32::try_from(s.health_score).unwrap_or(100));
        ui.set_checking(s.checking);
        ui.set_live_command(s.live_command.clone().into());
        if ui.get_query() != s.query.as_str() {
            ui.set_query(s.query.clone().into());
        }
        publish_summary(s);
    });
}

/// Row for an installed skill that the catalog does not list (no group, no update check).
fn local_row(s: &State, name: &str) -> SkillRow {
    let clients = s
        .installed
        .iter()
        .filter(|(_, names)| names.iter().any(|n| n == name))
        .map(|(agent, _)| agent.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let (health, detail) = s
        .health
        .get(name)
        .map_or((String::new(), String::new()), |(g, d)| {
            (g.clone(), d.clone())
        });
    SkillRow {
        name: name.into(),
        description: clients.into(),
        group: "".into(),
        installed: true,
        update: false,
        busy: s.busy == name,
        health: health.into(),
        health_detail: detail.into(),
    }
}

fn stale(s: &State, entry: &CatalogSkill) -> bool {
    let Some(recorded) = s.meta.get(&entry.name) else {
        return false;
    };
    let mut current = skill_catalog::skill_files(&s.tree, &entry.group, &entry.name);
    let mut recorded = recorded.clone();
    current.sort();
    recorded.sort();
    current != recorded
}
