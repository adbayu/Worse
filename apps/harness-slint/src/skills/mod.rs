//! Skill marketplace (spec `docs/specs/harness/skill-marketplace.md`): fetches the google/skills catalog,
//! caches it under `<data-dir>/skills-catalog/`, compares it with what each agent has on disk, and asks the
//! settings worker for a write plan. This module never writes an agent config file: it only projects rows into
//! `SkillsUi` and sends `Req::Plan` / `Req::SkillsInstalled`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use harness_agent_config::skill_catalog::{self, CatalogSkill, TreeEntry};
use harness_agent_config::skill_health::{self, Grade, Report};
use slint::{ComponentHandle, ModelRc, VecModel};
use tokio::sync::mpsc::UnboundedSender;

use crate::settings::{PlanOp, Req};
use crate::ui::{AppWindow, SkillRow, SkillsUi};

mod catalog;
mod health;
mod rows;

use catalog::{load_cache, save_meta, spawn_download, spawn_revalidate};
use health::check_health;
pub use health::health_loaded;
use rows::push;

const INDEX_URL: &str = "https://raw.githubusercontent.com/google/skills/main/index.json";
const TREE_URL: &str = "https://api.github.com/repos/google/skills/git/trees/main?recursive=1";
const RAW_ROOT: &str = "https://raw.githubusercontent.com/google/skills/main/";
const MAX_ROWS: usize = 24;
const REFRESH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

#[allow(clippy::struct_excessive_bools)] //独立的开关各自表示一种页面状态
struct State {
    window: slint::Weak<AppWindow>,
    req: UnboundedSender<Req>,
    cache: PathBuf,
    catalog: Vec<CatalogSkill>,
    tree: Vec<TreeEntry>,
    installed: HashMap<String, Vec<String>>,
    meta: HashMap<String, Vec<(String, String)>>,
    last_install: Option<(String, Vec<(String, String)>)>,
    /// Static health per installed skill name (spec `skill-health.md`); empty until checked.
    health: HashMap<String, (String, String)>,
    health_grade: String,
    health_score: u32,
    checking: bool,
    /// Command the user can copy when the live check is on (spec `skill-health.md` §Live check).
    live_command: String,
    query: String,
    busy: String,
    seq: u64,
    health_seq: u64,
    fetching: bool,
    last: Option<Instant>,
    status: String,
    status_error: bool,
    available: bool,
    started: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Read-only projection for the phone settings page (spec skill-marketplace.md §3.4): `(skill, agents)`.
static SUMMARY: std::sync::RwLock<Vec<(String, String)>> = std::sync::RwLock::new(Vec::new());

#[must_use]
pub fn summary() -> Vec<(String, String)> {
    SUMMARY.read().map(|s| s.clone()).unwrap_or_default()
}

pub fn install(window: &AppWindow, req: UnboundedSender<Req>, data_dir: &std::path::Path) {
    STATE.with(|cell| {
        *cell.borrow_mut() = Some(State {
            window: window.as_weak(),
            req,
            cache: data_dir.join("skills-catalog"),
            catalog: Vec::new(),
            tree: Vec::new(),
            installed: HashMap::new(),
            meta: HashMap::new(),
            last_install: None,
            health: HashMap::new(),
            health_grade: String::new(),
            health_score: 0,
            checking: false,
            live_command: String::new(),
            query: String::new(),
            busy: String::new(),
            seq: 0,
            health_seq: 0,
            fetching: false,
            last: None,
            status: String::new(),
            status_error: false,
            available: true,
            started: false,
        });
    });
    let ui = window.global::<SkillsUi>();
    ui.on_search(|q| search(&q));
    ui.on_refresh(|| refresh(true));
    ui.on_install(|name| install_skill(&name));
    ui.on_update(|name| install_skill(&name));
    ui.on_remove(|name| remove_skill(&name));
    ui.on_check_health(check_health);
}

pub fn open() {
    let job = STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let s = cell.as_mut()?;
        s.seq += 1;
        let seq = s.seq;
        let due = s.last.map_or(true, |t| t.elapsed() >= REFRESH_AFTER);
        let first = !s.started;
        s.started = true;
        Some((seq, due, s.cache.clone(), first))
    });
    let Some((seq, due, cache, first)) = job else {
        return;
    };
    loaded(seq);
    load_cache();
    push();
    if due || first {
        spawn_revalidate(cache);
    }
}

/// Asks the worker for the installed set; the answer arrives through [`installed_loaded`].
fn loaded(seq: u64) {
    let _ = STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|s| s.req.send(Req::SkillsInstalled { seq }))
    });
}

fn search(query: &str) {
    STATE.with(|cell| {
        if let Some(s) = cell.borrow_mut().as_mut() {
            s.query = query.to_string();
        }
    });
    push();
}

pub fn refresh(force: bool) {
    let job = STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let s = cell.as_mut()?;
        if !force && s.last.is_some_and(|t| t.elapsed() < REFRESH_AFTER) {
            return None;
        }
        s.seq += 1;
        Some((s.seq, s.cache.clone()))
    });
    let Some((seq, cache)) = job else {
        return;
    };
    loaded(seq);
    spawn_revalidate(cache);
}

fn install_skill(name: &str) {
    let job = STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let s = cell.as_mut()?;
        let entry = s.catalog.iter().find(|c| c.name == name)?.clone();
        s.busy = name.to_string();
        Some((entry, s.req.clone()))
    });
    let Some((entry, req)) = job else {
        return;
    };
    push();
    spawn_download(req, entry);
}

fn remove_skill(name: &str) {
    let job = STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let s = cell.as_mut()?;
        let installed = s.installed.values().any(|n| n.iter().any(|e| e == name));
        if !installed {
            return None;
        }
        s.busy = name.to_string();
        s.last_install = None;
        Some(s.req.clone())
    });
    if let Some(req) = job {
        push();
        let _ = req.send(Req::Plan {
            title: "remove skill",
            op: PlanOp::RemoveCatalog {
                name: name.to_string(),
            },
        });
    }
}

pub fn installed_loaded(seq: u64, installed: Vec<(String, Vec<String>)>) {
    let _ = slint::invoke_from_event_loop(move || {
        STATE.with(|cell| {
            let mut cell = cell.borrow_mut();
            let Some(s) = cell.as_mut() else {
                return;
            };
            if s.seq != seq {
                return;
            }
            s.installed = installed.into_iter().collect();
        });
        push();
    });
}

pub fn plan_finished(error: Option<&str>) {
    let error = error.map(str::to_string);
    STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(s) = cell.as_mut() else {
            return;
        };
        s.busy = String::new();
        if let Some(text) = error {
            s.status = text;
            s.status_error = true;
        } else {
            s.status = String::new();
            s.status_error = false;
            if let Some((name, shas)) = s.last_install.take() {
                s.meta.insert(name, shas);
            }
        }
    });
    save_meta();
    let seq = STATE.with(|cell| {
        cell.borrow_mut().as_ref().map(|s| {
            let seq = s.seq + 1;
            (seq, s.req.clone())
        })
    });
    if let Some((seq, req)) = seq {
        STATE.with(|cell| {
            if let Some(s) = cell.borrow_mut().as_mut() {
                s.seq = seq;
            }
        });
        let _ = req.send(Req::SkillsInstalled { seq });
    }
    push();
}

pub fn cancelled() {
    STATE.with(|cell| {
        if let Some(s) = cell.borrow_mut().as_mut() {
            s.busy = String::new();
            s.last_install = None;
        }
    });
    push();
}

// ---- cache ---------------------------------------------------------------------------------------------------
