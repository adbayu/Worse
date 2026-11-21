use super::{graphify, Command, PathBuf};
/// One command of a sequence, made when its turn comes: a later step can depend on what an earlier one
/// installed.
pub(super) type Step = Box<dyn FnOnce() -> Result<Command, String> + Send>;

/// What can install Graphify on this computer, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Installer {
    /// `uv tool install "graphifyy[mcp]==<pinned>"` into `<HARNESS_HOME>/tools`.
    Uv,
    /// `pipx install "graphifyy[mcp]"`.
    Pipx(PathBuf),
    /// Windows without uv: `winget install astral-sh.uv`, then as `Uv`. uv brings its own Python, so this
    /// works on a computer without Python.
    WingetThenUv(PathBuf),
    /// `python -m pip install --user "graphifyy[mcp]"`.
    Pip(PathBuf),
}

impl Installer {
    pub(super) fn find() -> Option<Self> {
        let on_path = harness_agent_hub::agents::resolve_program;
        if graphify::uv().is_some() {
            return Some(Self::Uv);
        }
        if let Some(pipx) = on_path("pipx") {
            return Some(Self::Pipx(pipx));
        }
        if cfg!(windows) {
            if let Some(winget) = on_path("winget") {
                return Some(Self::WingetThenUv(winget));
            }
        }
        ["python3", "python", "py"]
            .into_iter()
            .find_map(on_path)
            .map(Self::Pip)
    }

    /// What it will run, in words, for the page.
    pub(super) fn plan(&self) -> String {
        match self {
            Self::Uv => graphify::INSTALL_COMMAND.to_owned(),
            Self::Pipx(_) => format!("pipx install \"{}\"", graphify::PACKAGE),
            Self::WingetThenUv(_) => {
                format!(
                    "winget install astral-sh.uv, then {}",
                    graphify::INSTALL_COMMAND
                )
            }
            Self::Pip(_) => format!("python -m pip install --user \"{}\"", graphify::PACKAGE),
        }
    }

    pub(super) fn steps(self) -> Vec<Step> {
        fn with_uv() -> Result<Command, String> {
            let uv = graphify::uv().ok_or(
                "uv was installed but is not found yet. Restart Harness, then press Set up again.",
            )?;
            let mut command = Command::new(uv);
            command.args(["tool", "install", "--force", graphify::PACKAGE]);
            if let (Some(tools), Some(bin)) = (graphify::own_tool_dir(), graphify::own_bin_dir()) {
                command
                    .env("UV_TOOL_DIR", tools)
                    .env("UV_TOOL_BIN_DIR", bin)
                    .env("UV_NO_MODIFY_PATH", "1");
            }
            Ok(command)
        }
        match self {
            Self::Uv => vec![Box::new(with_uv)],
            Self::Pipx(pipx) => vec![Box::new(move || {
                let mut command = Command::new(pipx);
                command.args(["install", graphify::PACKAGE, "--force"]);
                Ok(command)
            })],
            Self::WingetThenUv(winget) => vec![
                Box::new(move || {
                    let mut command = Command::new(winget);
                    command.args([
                        "install",
                        "--id",
                        "astral-sh.uv",
                        "-e",
                        "--accept-source-agreements",
                        "--accept-package-agreements",
                        "--disable-interactivity",
                    ]);
                    Ok(command)
                }),
                Box::new(with_uv),
            ],
            Self::Pip(python) => vec![Box::new(move || {
                let mut command = Command::new(python);
                command.args(["-m", "pip", "install", "--user", graphify::PACKAGE]);
                Ok(command)
            })],
        }
    }
}
