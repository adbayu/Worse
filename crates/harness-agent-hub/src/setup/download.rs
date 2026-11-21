//! Downloads a missing agent CLI or Node.js into `<home>/tools` for an install (see [`crate::tools`]).

use std::path::PathBuf;

use super::{add_search_dir, npm_command, AdapterInstallStatus, AgentId, Hub};

impl Hub {
    fn progress_sink<'a>(
        &'a self,
        agent: &'a AgentId,
        package: &'a str,
    ) -> impl Fn(String) + Send + Sync + 'a {
        move |line| {
            self.install_event(
                agent,
                package,
                AdapterInstallStatus::Running,
                Some(line),
                None,
            );
        }
    }

    /// Downloads `cli` from its official channel. `Err` carries the message for the user.
    pub(super) async fn download_cli(
        &self,
        agent: &AgentId,
        package: &str,
        cli: &str,
    ) -> Result<PathBuf, String> {
        if !crate::tools::downloadable(cli) {
            return Err(format!("install the `{cli}` CLI first"));
        }
        let Some(_slot) = super::InstallSlot::take(cli) else {
            return Err(format!("`{cli}` is already being downloaded"));
        };
        tracing::info!(%cli, "cli download started");
        let sink = self.progress_sink(agent, package);
        let path = crate::tools::download_cli(cli, &sink).await?;
        if let Some(dir) = path.parent() {
            add_search_dir(dir.to_path_buf());
        }
        tracing::info!(%cli, path = %path.display(), "cli download finished");
        Ok(path)
    }

    /// npm on PATH, else the Harness Node.js (downloaded on first use).
    pub(super) async fn npm_or_download(
        &self,
        agent: &AgentId,
        package: &str,
    ) -> Result<PathBuf, String> {
        if let Some(npm) = npm_command() {
            return Ok(npm);
        }
        if std::env::var_os(super::ENV_NPM).is_some_and(|v| !v.is_empty()) {
            return Err(super::NPM_STEPS.to_owned());
        }
        let Some(_slot) = super::InstallSlot::take("node") else {
            return Err(
                "Node.js is already being downloaded; try again when it finishes".to_owned(),
            );
        };
        let sink = self.progress_sink(agent, package);
        let npm = crate::tools::download_node(&sink)
            .await
            .map_err(|e| format!("{e}. {}", super::NPM_STEPS))?;
        if let Some(dir) = npm.parent() {
            add_search_dir(dir.to_path_buf());
        }
        Ok(npm)
    }
}
