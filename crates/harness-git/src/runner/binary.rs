use super::{
    exec, js_trim, never_cancelled, Command, DISCOVERY_TIMEOUT, FORCED_ENV, GIT_LOCAL_ENV_VARS,
};

pub(super) fn candidates() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(value) = std::env::var("ZCODE_GIT_BINARY") {
        let value = js_trim(&value);
        if !value.is_empty() {
            out.push(value.to_owned());
        }
    }
    out.push("git".to_owned());
    if cfg!(windows) {
        let program_files = std::env::var("ProgramW6432")
            .or_else(|_| std::env::var("ProgramFiles"))
            .unwrap_or_else(|_| "C:\\Program Files".to_owned());
        let program_files_x86 = std::env::var("ProgramFiles(x86)")
            .unwrap_or_else(|_| "C:\\Program Files (x86)".to_owned());
        for base in [
            &program_files,
            &program_files,
            &program_files_x86,
            &program_files_x86,
        ]
        .iter()
        .zip(["cmd", "bin", "cmd", "bin"])
        {
            out.push(harness_fs::node_join(
                &harness_fs::node_join(&harness_fs::node_join(base.0, "Git"), base.1),
                "git.exe",
            ));
        }
    }
    out
}

pub(super) fn base_command(binary: &str) -> Command {
    let mut command = Command::new(binary);
    for name in GIT_LOCAL_ENV_VARS {
        command.env_remove(name);
    }
    for (name, value) in FORCED_ENV {
        command.env(name, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// `git --version` succeeds within the discovery timeout (same event-driven
/// wait as [`exec`]; the old 5ms sleep loop cost a full timer tick on Windows).
pub(super) fn probe(candidate: &str) -> bool {
    exec(
        candidate,
        None,
        &["--version"],
        DISCOVERY_TIMEOUT,
        64 * 1024,
        &never_cancelled,
    )
    .is_ok_and(|result| result.exit_code == Some(0))
}
