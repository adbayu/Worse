use super::{
    add_search_dir, clip, global_bin_dir, known_cli, npm, npm_command, resolve_program,
    AdapterInstallResult, AdapterInstallStatus, AgentId, ApiError, ApiResult, AsyncBufReadExt,
    Availability, BufReader, EventKind, GhSetup, HashSet, Hub, InstallSlot, Path, SetupAgent,
    SetupState, SetupStatus, INSTALL_TIMEOUT, NPM_STEPS,
};

impl Hub {
    pub(crate) fn do_setup_status(&self) -> SetupStatus {
        let mut agents: Vec<SetupAgent> = Vec::new();
        let mut seen = HashSet::new();
        for spec in &self.ctx.config.agents {
            let Some(known) = known_cli(&spec.program) else {
                continue;
            };
            if !seen.insert(known.program) {
                continue;
            }
            let cli_path = resolve_program(known.cli).map(|p| p.to_string_lossy().into_owned());
            let state = match spec.probe() {
                Availability::Available { .. } => SetupState::Ready,
                Availability::AdapterMissing { .. } => SetupState::AdapterMissing,
                Availability::NotInstalled { .. } => SetupState::CliMissing,
            };
            agents.push(SetupAgent {
                agent: spec.id.clone(),
                display_name: spec.display_name.clone(),
                cli: known.cli.to_owned(),
                cli_path,
                package: known.package.map(str::to_owned),
                state,
            });
        }
        SetupStatus {
            npm_path: npm_command().map(|p| p.to_string_lossy().into_owned()),
            npm_steps: NPM_STEPS.to_owned(),
            agents,
            gh: crate::gh::resolve_gh().map_or_else(GhSetup::default, |(path, bundled)| GhSetup {
                path: Some(path.to_string_lossy().into_owned()),
                bundled,
            }),
        }
    }

    pub(crate) async fn do_install_gh(&self) -> ApiResult<()> {
        let agent = AgentId::new("gh");
        let package = "cli/cli";
        if crate::gh::resolve_gh().is_some() {
            self.install_event(
                &agent,
                package,
                AdapterInstallStatus::Succeeded,
                None,
                Some("GitHub CLI is already installed".to_owned()),
            );
            return Ok(());
        }
        let Some(_guard) = crate::gh::InstallGuard::take() else {
            return Err(ApiError::Busy(
                "GitHub CLI is already being installed".to_owned(),
            ));
        };
        tracing::info!("gh install started");
        let result = crate::gh::install(|line| {
            self.install_event(
                &agent,
                package,
                AdapterInstallStatus::Running,
                Some(line),
                None,
            );
        })
        .await;
        match result {
            Ok(path) => {
                tracing::info!(path = %path.display(), "gh install finished");
                self.install_event(
                    &agent,
                    package,
                    AdapterInstallStatus::Succeeded,
                    None,
                    Some(format!("GitHub CLI installed at {}", path.display())),
                );
            }
            Err(message) => {
                tracing::warn!(%message, "gh install failed");
                self.install_event(
                    &agent,
                    package,
                    AdapterInstallStatus::Failed,
                    None,
                    Some(message),
                );
            }
        }
        if let Ok(mut slot) = self.inbox.gh.lock() {
            *slot = None;
        }
        self.ctx.events.emit(None, EventKind::InboxChanged);
        Ok(())
    }

    pub(crate) fn install_event(
        &self,
        agent: &AgentId,
        package: &str,
        status: AdapterInstallStatus,
        line: Option<String>,
        message: Option<String>,
    ) {
        self.ctx.events.emit(
            None,
            EventKind::AdapterInstall {
                agent: agent.clone(),
                package: package.to_owned(),
                status,
                line,
                message,
            },
        );
    }

    pub(super) fn install_done(
        &self,
        agent: &AgentId,
        package: &str,
        ok: bool,
        message: String,
    ) -> AdapterInstallResult {
        let status = if ok {
            AdapterInstallStatus::Succeeded
        } else {
            AdapterInstallStatus::Failed
        };
        self.install_event(agent, package, status, None, Some(message.clone()));
        AdapterInstallResult {
            agent: agent.clone(),
            ok,
            message,
        }
    }

    pub(crate) async fn do_install_adapter(
        &self,
        agent: AgentId,
    ) -> ApiResult<AdapterInstallResult> {
        let spec = self
            .ctx
            .specs
            .get(&agent)
            .cloned()
            .ok_or_else(|| ApiError::NotFound(format!("agent {agent}")))?;
        let known = known_cli(&spec.program)
            .filter(|k| k.package.is_some())
            .ok_or_else(|| {
                ApiError::InvalidArgument(format!(
                    "{} has no ACP adapter to install",
                    spec.display_name
                ))
            })?;
        let package = known.package.unwrap_or_default();
        match spec.probe() {
            Availability::Available { .. } => {
                return Ok(AdapterInstallResult {
                    agent,
                    ok: true,
                    message: format!("{package} is already installed"),
                })
            }
            Availability::NotInstalled { .. } => {
                return Err(ApiError::InvalidArgument(format!(
                    "install the `{}` CLI first",
                    known.cli
                )))
            }
            Availability::AdapterMissing { .. } => {}
        }
        let Some(npm_path) = npm_command() else {
            return Ok(self.install_done(&agent, package, false, NPM_STEPS.to_owned()));
        };
        let Some(_slot) = InstallSlot::take(package) else {
            return Err(ApiError::Busy(format!(
                "{package} is already being installed"
            )));
        };
        tracing::info!(%package, "adapter install started");
        let code = self.run_npm_install(&agent, package, &npm_path).await;
        tracing::info!(%package, ?code, "adapter install finished");
        let code = match code {
            Ok(code) => code,
            Err(message) => return Ok(self.install_done(&agent, package, false, message)),
        };
        if code != Some(0) {
            let message = format!(
                "npm install -g {package} failed (exit code {})",
                code.map_or_else(|| "none".to_owned(), |c| c.to_string())
            );
            return Ok(self.install_done(&agent, package, false, message));
        }
        let bin = global_bin_dir(&npm_path).await;
        if let Some(dir) = &bin {
            add_search_dir(dir.clone());
        }
        if spec.probe().is_available() {
            return Ok(self.install_done(&agent, package, true, format!("{package} installed")));
        }
        let place = bin.map_or_else(
            || "the npm global directory".to_owned(),
            |d| d.display().to_string(),
        );
        Ok(self.install_done(
            &agent,
            package,
            false,
            format!(
                "npm installed {package} into {place}, but `{}` is still not found; restart Harness or add that directory to PATH",
                known.program
            ),
        ))
    }

    /// Runs `npm install -g <package>`, streaming every output line as an event. `Ok(exit code)` or a message.
    pub(crate) async fn run_npm_install(
        &self,
        agent: &AgentId,
        package: &str,
        npm_path: &Path,
    ) -> Result<Option<i32>, String> {
        let mut child = npm(npm_path, &["install", "-g", package])
            .spawn()
            .map_err(|e| format!("cannot start npm: {e}"))?;
        let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        if let Some(stdout) = child.stdout.take() {
            let tx = line_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = tx.send(line);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let tx = line_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = tx.send(line);
                }
            });
        }
        drop(line_tx);
        let run = async {
            while let Some(line) = line_rx.recv().await {
                if line.trim().is_empty() {
                    continue;
                }
                let line = clip(&line);
                tracing::debug!(%package, %line, "npm");
                self.install_event(
                    agent,
                    package,
                    AdapterInstallStatus::Running,
                    Some(line),
                    None,
                );
            }
            child.wait().await
        };
        match tokio::time::timeout(INSTALL_TIMEOUT, run).await {
            Ok(Ok(status)) => Ok(status.code()),
            Ok(Err(error)) => Err(format!("npm failed: {error}")),
            Err(_) => Err(format!(
                "npm install -g {package} did not finish within {} minutes and was stopped",
                INSTALL_TIMEOUT.as_secs() / 60
            )),
        }
    }
}
