//! Dependency audit (spec `docs/specs/harness/security-panel.md`): owns the `osv-scanner` child process, mirrors the
//! last report into `SecurityUi`, and answers the commit gate from the cached report.

pub mod binary;
pub mod gate;
pub mod lockfiles;
pub mod report;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};

use crate::security::gate::Verdict;
use crate::security::report::Vuln;
use crate::ui::{AppWindow, SecurityUi, VulnRow};

const SCAN_LIMIT: Duration = Duration::from_secs(180);
const DEBOUNCE: Duration = Duration::from_secs(3);
/// Poll interval while the scanner child runs.
const STALL: Duration = Duration::from_millis(100);
/// Upper bound for reading the scanner's pipes after it exited.
const DRAIN_LIMIT: Duration = Duration::from_secs(10);

enum Cmd {
    Scan { root: PathBuf, offline: bool },
    Download,
    Remove,
}

struct State {
    window: Weak<AppWindow>,
    home: PathBuf,
    root: Option<PathBuf>,
    tx: Option<mpsc::Sender<Cmd>>,
    /// Lockfile debounce clock; dropped with the state so the timer stops on teardown.
    clock: slint::Timer,
    installed: bool,
    scanning: bool,
    busy: bool,
    rows: Vec<Vuln>,
    report: Option<Result<Vec<Vuln>, String>>,
    scanned_at: Option<Instant>,
    observed: Option<SystemTime>,
    status: String,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn sv(text: &str) -> SharedString {
    SharedString::from(text)
}

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

/// Wires the Security page callbacks and starts the lockfile watcher.
pub fn install(window: &AppWindow, home: &Path) {
    let installed = binary::installed(home).is_some();
    STATE.with(|cell| {
        *cell.borrow_mut() = Some(State {
            window: window.as_weak(),
            home: home.to_path_buf(),
            root: None,
            tx: None,
            clock: slint::Timer::default(),
            installed,
            scanning: false,
            busy: false,
            rows: Vec::new(),
            report: None,
            scanned_at: None,
            observed: None,
            status: String::new(),
        });
    });
    let ui = window.global::<SecurityUi>();
    ui.on_download(|| spawn(Cmd::Download));
    ui.on_remove(|| spawn(Cmd::Remove));
    ui.on_scan(|| request(true));
    ui.on_set_gate(|gate| {
        crate::settings::patch_setting(
            "security.commitGate",
            serde_json::Value::String(gate.to_string()),
        );
    });
    let clock = slint::Timer::default();
    clock.start(slint::TimerMode::Repeated, DEBOUNCE, watch_lockfiles);
    with_state(|state| state.clock = clock);
    push();
}

/// Follows the session's workspace. The first visit only records the lockfile timestamps.
pub fn set_workspace(root: &Path) {
    let changed = with_state(|state| {
        if state.root.as_deref() == Some(root) {
            return false;
        }
        state.root = Some(root.to_path_buf());
        state.observed = lockfiles::newest(root);
        state.rows.clear();
        state.report = None;
        state.scanned_at = None;
        true
    })
    .unwrap_or(false);
    if changed {
        push();
        request(false);
    }
}

pub fn teardown() {
    STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().take() {
            drop(state.tx);
        }
    });
}

fn gate_setting() -> String {
    crate::settings::current().map_or_else(
        || "off".to_owned(),
        |s| format!("{:?}", s.security.commit_gate).to_lowercase(),
    )
}

fn auto_scan() -> bool {
    crate::settings::current().is_some_and(|s| s.security.scan_on_lockfile_change)
}

fn offline_scan() -> bool {
    crate::settings::current().is_some_and(|s| s.security.offline_scan)
}

/// Debounced trigger: a lockfile newer than the last scan starts one scan, and only when enabled.
fn watch_lockfiles() {
    let due = with_state(|state| -> Option<()> {
        if state.scanning {
            return None;
        }
        let root = state.root.clone()?;
        let newest = lockfiles::newest(&root)?;
        if state.observed.is_some_and(|seen| newest <= seen) {
            return None;
        }
        state.observed = Some(newest);
        Some(())
    });
    if due.is_some() && auto_scan() {
        request(false);
    }
}

/// Starts a scan: `Scan now` (`force`) or a settings-driven trigger. `spawn` owns the busy flag.
fn request(force: bool) {
    let root = with_state(|state| {
        if state.scanning || (!force && !auto_scan()) {
            return None;
        }
        state.root.clone()
    });
    let Some(Some(root)) = root else {
        return;
    };
    spawn(Cmd::Scan {
        root,
        offline: offline_scan(),
    });
}

fn spawn(cmd: Cmd) {
    let queued = with_state(|state| {
        match cmd {
            Cmd::Scan { .. } => {
                if state.scanning {
                    return false;
                }
                state.scanning = true;
            }
            Cmd::Download | Cmd::Remove => {
                if state.busy {
                    return false;
                }
                state.busy = true;
            }
        }
        if state.tx.is_none() {
            let (tx, rx) = mpsc::channel::<Cmd>();
            let home = state.home.clone();
            let spawned = std::thread::Builder::new()
                .name("harness-security".into())
                .spawn(move || worker::run(&home, &rx))
                .is_ok();
            if !spawned {
                state.scanning = false;
                state.busy = false;
                state.status = "cannot start the security worker".into();
                return false;
            }
            state.tx = Some(tx);
        }
        state.tx.as_ref().is_some_and(|tx| tx.send(cmd).is_ok())
    });
    if queued != Some(true) {
        with_state(|state| {
            state.scanning = false;
            state.busy = false;
        });
    }
    push();
}

fn push() {
    let payload = with_state(|state| {
        let rows: Vec<VulnRow> = state.rows.iter().map(row).collect();
        (
            state.window.clone(),
            rows,
            report::counts(&state.rows),
            state.installed,
            state.scanning || state.busy,
            state.status.clone(),
            state.scanned_at.is_some(),
        )
    });
    let Some((window, rows, counts, installed, working, status, scanned)) = payload else {
        return;
    };
    let gate = gate_setting();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(window) = window.upgrade() else {
            return;
        };
        let ui = window.global::<SecurityUi>();
        let count = |value: usize| i32::try_from(value).unwrap_or(i32::MAX);
        ui.set_rows(ModelRc::from(std::rc::Rc::new(VecModel::from(rows))));
        ui.set_total(count(counts.iter().sum()));
        ui.set_critical(count(counts[0]));
        ui.set_high(count(counts[1]));
        ui.set_medium(count(counts[2]));
        ui.set_low(count(counts[3]));
        ui.set_installed(installed);
        ui.set_scanning(working);
        ui.set_version(sv(if installed { binary::VERSION } else { "" }));
        ui.set_gate(sv(&gate));
        ui.set_status(sv(&status));
        ui.set_error(sv(&status));
        ui.set_scanned_at(sv(if scanned { "just now" } else { "" }));
    });
}

fn row(vuln: &Vuln) -> VulnRow {
    VulnRow {
        package: sv(&vuln.package),
        version: sv(&vuln.version),
        id: sv(&vuln.id),
        severity: sv(&vuln.severity),
        fixed: sv(if vuln.fixed.is_empty() {
            "no fix"
        } else {
            &vuln.fixed
        }),
        source: sv(vuln
            .source
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&vuln.source)),
    }
}

/// Commit gate for one commit: the cached report plus `security.commitGate`. Never touches the network.
#[must_use]
pub fn verdict(changed: &[String]) -> Verdict {
    let setting = gate_setting();
    let cached = with_state(|state| state.report.clone()).flatten();
    match cached {
        Some(Ok(rows)) => gate::decide(&setting, changed, Ok(&rows)),
        Some(Err(reason)) => gate::decide(&setting, changed, Err(&reason)),
        None => gate::decide(&setting, changed, Err("no dependency scan has run yet")),
    }
}

/// The last report, for the diff file badge.
#[must_use]
pub fn rows() -> Vec<Vuln> {
    with_state(|state| state.rows.clone()).unwrap_or_default()
}

fn blocking_download(home: &Path) -> Result<PathBuf, String> {
    let home = home.to_path_buf();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the download: {e}"))?
        .block_on(binary::ensure(&home))
}

enum Kind {
    Scan,
    Tool,
}

fn finished(result: Result<Vec<Vuln>, String>, kind: &Kind) {
    with_state(|state| {
        state.scanning = false;
        state.busy = false;
        if matches!(kind, Kind::Tool) {
            state.installed = binary::installed(&state.home).is_some();
            state.status = result.err().unwrap_or_default();
            return;
        }
        match result {
            Ok(rows) => {
                state.status = if rows.is_empty() {
                    "No known vulnerabilities".to_owned()
                } else {
                    String::new()
                };
                state.rows.clone_from(&rows);
                state.report = Some(Ok(rows));
                state.scanned_at = Some(Instant::now());
                state.observed = state.root.as_deref().and_then(lockfiles::newest);
            }
            Err(error) => {
                state.status.clone_from(&error);
                state.rows.clear();
                state.report = Some(Err(error));
            }
        }
    });
    push();
}

mod worker;
