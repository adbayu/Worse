//! First-run setup dialog (phase-9-parity.md §9.10).
//!
//! Owner: this module owns the dialog state (`SetupUi`): rows from the hub's `setup_status`, the running install,
//! its last output lines and the install queue of "Install missing adapters". The hub owns detection and the npm
//! process; the settings store owns the `setup.firstRunDone` flag (written through `settings::patch_setting`).

use std::cell::RefCell;
use std::collections::VecDeque;

use harness_core_api as api;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::core::{Command, SetupCall};
use crate::ui::{AppWindow, SetupRow, SetupUi};

const LOG_LINES: usize = 8;
const FLAG_PATH: &str = "setup.firstRunDone";
const GH: &str = "gh";

struct State {
    window: AppWindow,
    rows: Vec<api::SetupAgent>,
    gh: api::GhSetup,
    /// Agent of the running install.
    busy: Option<String>,
    /// Agents still to install after the running one ("Install missing adapters").
    queue: VecDeque<String>,
    log: VecDeque<String>,
    /// `setup.firstRunDone` was false at startup and the first status has not been decided yet.
    first_run_pending: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state(f: impl FnOnce(&mut State)) {
    STATE.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(state) = guard.as_mut() {
                f(state);
            }
        }
    });
}

fn sv(s: impl AsRef<str>) -> SharedString {
    SharedString::from(s.as_ref())
}

/// A row Harness can install: a missing adapter, or a missing CLI it downloads itself.
fn installable(row: &api::SetupAgent) -> bool {
    match row.state {
        api::SetupState::AdapterMissing => true,
        api::SetupState::CliMissing => harness_agent_hub::setup::cli_downloadable(&row.cli),
        api::SetupState::Ready => false,
    }
}

fn request_status() {
    crate::bridge::send(Command::Setup(SetupCall::Status));
}

impl State {
    fn ui(&self) -> SetupUi<'_> {
        self.window.global::<SetupUi>()
    }

    fn render(&self) {
        let ui = self.ui();
        let mut rows: Vec<SetupRow> = self
            .rows
            .iter()
            .map(|r| SetupRow {
                agent: sv(r.agent.as_str()),
                name: sv(&r.display_name),
                cli: sv(&r.cli),
                cli_path: sv(r.cli_path.as_deref().unwrap_or_default()),
                package: sv(r.package.as_deref().unwrap_or_default()),
                state: match r.state {
                    api::SetupState::Ready => 0,
                    api::SetupState::AdapterMissing => 1,
                    api::SetupState::CliMissing if installable(r) => 3,
                    api::SetupState::CliMissing => 2,
                },
            })
            .collect();
        rows.push(SetupRow {
            agent: sv(GH),
            name: sv("GitHub CLI (gh)"),
            cli: sv("gh"),
            cli_path: sv(self.gh.path.as_deref().map_or_else(String::new, |p| {
                if self.gh.bundled {
                    format!("{p} (Harness copy)")
                } else {
                    p.to_owned()
                }
            })),
            package: sv("cli/cli"),
            state: if self.gh.path.is_some() { 0 } else { 3 },
        });
        ui.set_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_busy(sv(self.busy.as_deref().unwrap_or_default()));
        ui.set_any_missing(self.gh.path.is_none() || self.rows.iter().any(installable));
        let log: Vec<&str> = self.log.iter().map(String::as_str).collect();
        ui.set_log(sv(log.join("\n")));
    }

    fn push_log(&mut self, line: String) {
        self.log.push_back(line);
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }

    fn start(&mut self, agent: String) {
        if self.busy.is_some() {
            return;
        }
        self.log.clear();
        self.ui().set_error(SharedString::new());
        self.busy = Some(agent.clone());
        self.render();
        crate::bridge::send(Command::Setup(SetupCall::Install(agent)));
    }

    /// Installs every missing tool one after another.
    fn install_missing(&mut self) {
        let gh = self.gh.path.is_none().then(|| GH.to_owned());
        let mut missing = gh.into_iter().chain(
            self.rows
                .iter()
                .filter(|r| installable(r))
                .map(|r| r.agent.0.clone()),
        );
        if let Some(first) = missing.next() {
            self.queue = missing.collect();
            self.start(first);
        }
    }

    fn close(&mut self) {
        self.queue.clear();
        self.ui().set_open(false);
        self.ui().set_installed_any(false);
        // §9.10：Done / Skip for now / X 都记下首次设置已处理，之后启动不再弹出。
        crate::settings::patch_setting(FLAG_PATH, serde_json::Value::Bool(true));
    }
}

/// Installs the dialog state and callbacks. `first_run_done` is `setup.firstRunDone` of the startup snapshot.
pub fn install(window: &AppWindow, first_run_done: bool) {
    let ui = window.global::<SetupUi>();
    ui.on_open_setup(|| {
        with_state(|s| {
            s.ui().set_open(true);
            s.render();
        });
        request_status();
    });
    ui.on_install(|agent| with_state(|s| s.start(agent.to_string())));
    ui.on_install_missing(|| with_state(State::install_missing));
    ui.on_close(|| with_state(State::close));
    STATE.with(|cell| {
        *cell.borrow_mut() = Some(State {
            window: window.clone_strong(),
            rows: Vec::new(),
            gh: api::GhSetup::default(),
            busy: None,
            queue: VecDeque::new(),
            log: VecDeque::new(),
            first_run_pending: !first_run_done,
        });
    });
    // 首次启动检查：事件循环启动、bridge 安装完后再请求状态。
    slint::Timer::single_shot(std::time::Duration::ZERO, request_status);
}

/// Setup rows from the hub (`Command::Setup(Status)`, and after every install).
pub fn status(result: Result<api::SetupStatus, String>) {
    with_state(|s| {
        let status = match result {
            Ok(status) => status,
            Err(error) => {
                s.ui().set_error(sv(error));
                return;
            }
        };
        s.ui().set_npm_ok(status.npm_path.is_some());
        s.ui().set_npm_steps(sv(&status.npm_steps));
        s.rows = status.agents;
        s.gh = status.gh;
        let mut auto_install = false;
        if s.first_run_pending {
            s.first_run_pending = false;
            let missing = s.rows.iter().any(installable);
            let none_ready =
                !s.rows.is_empty() && !s.rows.iter().any(|r| r.state == api::SetupState::Ready);
            let gh_missing = s.gh.path.is_none();
            if missing || none_ready || gh_missing {
                s.ui().set_open(true);
                auto_install = true;
            } else {
                // 所有适配器都在（或没有已知 CLI 的 agent）：静默记下，不打扰用户。
                crate::settings::patch_setting(FLAG_PATH, serde_json::Value::Bool(true));
            }
        }
        s.render();
        if auto_install {
            s.install_missing();
        }
    });
}

/// Progress of an adapter install (hub `AdapterInstall` event).
pub fn install_event(
    agent: &str,
    status: api::AdapterInstallStatus,
    line: Option<String>,
    message: Option<String>,
) {
    with_state(|s| {
        if let Some(line) = line {
            s.push_log(line);
        }
        if status == api::AdapterInstallStatus::Running {
            s.render();
            return;
        }
        if s.busy.as_deref() == Some(agent) {
            s.busy = None;
        }
        if status == api::AdapterInstallStatus::Succeeded {
            s.ui().set_installed_any(true);
            if let Some(message) = message {
                s.push_log(message);
            }
        } else {
            s.ui().set_error(sv(message.unwrap_or_default()));
        }
        s.render();
        request_status();
        if let Some(next) = s.queue.pop_front() {
            s.start(next);
        }
    });
}
