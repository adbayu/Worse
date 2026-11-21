//! Agent adapters (spec §6). Round 8.1: every agent speaks ACP over stdio; adapters differ in launch
//! command and error classification.

use std::path::{Path, PathBuf};

use harness_core_api::{AgentId, Availability, ErrorClass};

use crate::quota::{classify_generic, AgentErrorInfo};

/// Wire protocol of an agent process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Agent Client Protocol over stdio (JSON-RPC, newline-delimited).
    Acp,
}

/// How errors of this agent are classified (spec §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorDialect {
    /// Claude Code (through claude-agent-acp): usage-limit texts `Claude AI usage limit reached|<unix>`.
    ClaudeCode,
    /// Codex (through codex-acp): `codexErrorInfo` = `usageLimitExceeded` / `rateLimitExceeded`.
    Codex,
    /// agy: format unknown, generic rules only.
    Agy,
    /// Harness convention `data.harnessErrorKind` + generic text rules (fake agents, extra agents).
    Generic,
}

/// Extra launch data of the code mode variant of an agent, appended to the normal launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeModeLaunch {
    pub env: Vec<(String, String)>,
    pub args: Vec<String>,
}

/// One configured agent. `Debug` prints env names only (values may be API keys, §9.7).
#[derive(Clone)]
pub struct AgentSpec {
    pub id: AgentId,
    pub display_name: String,
    /// Command name (PATH lookup) or absolute path.
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub install_hint: String,
    pub transport: Transport,
    pub dialect: ErrorDialect,
}

impl std::fmt::Debug for AgentSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env: Vec<&str> = self.env.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("AgentSpec")
            .field("id", &self.id)
            .field("display_name", &self.display_name)
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env", &env)
            .field("transport", &self.transport)
            .field("dialect", &self.dialect)
            .finish_non_exhaustive()
    }
}

impl AgentSpec {
    /// Resolves availability by PATH lookup only (never runs the CLI).
    #[must_use]
    pub fn probe(&self) -> Availability {
        match resolve_program(&self.program) {
            Some(path) => Availability::Available {
                command_path: path.to_string_lossy().into_owned(),
            },
            // §9.10：适配器缺失但 CLI 已安装时报告 AdapterMissing，而不是笼统的 "Not installed"。
            None => crate::setup::probe_missing(self),
        }
    }

    /// Classifies an agent failure (quota / transient / fatal).
    #[must_use]
    pub fn classify_error(&self, info: &AgentErrorInfo) -> ErrorClass {
        classify_generic(info, self.dialect)
    }

    /// Launch data of the code mode variant of this agent, `None` when the agent has no code mode.
    #[must_use]
    pub fn code_mode_launch(&self) -> Option<CodeModeLaunch> {
        match self.dialect {
            ErrorDialect::ClaudeCode => Some(CodeModeLaunch {
                env: vec![(ENV_CLAUDE_CODE_MODE.into(), "on".into())],
                args: Vec::new(),
            }),
            ErrorDialect::Codex => Some(CodeModeLaunch {
                env: Vec::new(),
                args: vec!["-c".into(), "features.code_mode=true".into()],
            }),
            ErrorDialect::Agy => None,
            ErrorDialect::Generic => self
                .args
                .iter()
                .any(|arg| arg == crate::helpers::FLAG_API_AGENT)
                .then(|| CodeModeLaunch {
                    env: vec![(ENV_HARNESS_CODE_MODE.into(), "1".into())],
                    args: Vec::new(),
                }),
        }
    }
}

/// Opt-in switch of the native code mode of [CC].
pub const ENV_CLAUDE_CODE_MODE: &str = "CLAUDE_CODE_EXPERIMENTAL_CODE_MODE";
/// Switch read by the Harness API agent helper (`<exe> --api-agent`).
pub const ENV_HARNESS_CODE_MODE: &str = "HARNESS_CODE_MODE";

/// Built-in agents in default failover order, from `harness-config` `assets/agents.toml`.
#[must_use]
pub fn builtin_agents() -> Vec<AgentSpec> {
    harness_config::builtin_agents()
        .into_iter()
        .map(|agent| AgentSpec {
            id: AgentId::new(agent.id),
            display_name: agent.display_name,
            program: agent.program,
            args: agent.args,
            env: Vec::new(),
            install_hint: agent.install_hint,
            transport: Transport::Acp,
            dialect: agent.dialect.into(),
        })
        .collect()
}

impl From<harness_config::Dialect> for ErrorDialect {
    fn from(dialect: harness_config::Dialect) -> Self {
        match dialect {
            harness_config::Dialect::ClaudeCode => Self::ClaudeCode,
            harness_config::Dialect::Codex => Self::Codex,
            harness_config::Dialect::Agy => Self::Agy,
            harness_config::Dialect::Generic => Self::Generic,
        }
    }
}

/// Agent id of the built-in Command Code agent (the one that uses the CLI's own login).
pub const COMMAND_CODE: &str = "command-code";

/// Finds an executable: absolute/relative paths as-is, bare names on PATH (with PATHEXT on Windows).
#[must_use]
pub fn resolve_program(program: &str) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 || candidate.is_absolute() {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    // §9.10：PATH 之后再查安装适配器后发现的 npm 全局 bin 目录，免重启即可用。
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(crate::setup::extra_search_dirs());
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_ascii_lowercase)
            .collect()
    } else {
        Vec::new()
    };
    for dir in dirs {
        if cfg!(windows) {
            for ext in &exts {
                let file = dir.join(format!("{program}{ext}"));
                if file.is_file() {
                    return Some(file);
                }
            }
        } else {
            let file = dir.join(program);
            if file.is_file() {
                return Some(file);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn builtin(id: &str) -> AgentSpec {
        builtin_agents()
            .into_iter()
            .find(|a| a.id.as_str() == id)
            .unwrap()
    }

    #[test]
    fn claude_code_has_a_code_mode_variant() {
        let launch = builtin("claude-code").code_mode_launch().unwrap();
        assert_eq!(
            launch.env,
            [(ENV_CLAUDE_CODE_MODE.to_owned(), "on".to_owned())]
        );
        assert_eq!(launch.args.len(), 0);
    }

    #[test]
    fn codex_has_a_code_mode_variant() {
        let launch = builtin("codex").code_mode_launch().unwrap();
        assert_eq!(launch.args, ["-c", "features.code_mode=true"]);
        assert_eq!(launch.env.len(), 0);
    }

    #[test]
    fn agy_and_command_code_have_no_code_mode_variant() {
        assert!(builtin("agy").code_mode_launch().is_none());
        assert!(builtin(COMMAND_CODE).code_mode_launch().is_none());
    }

    #[test]
    fn an_api_agent_gets_the_harness_switch() {
        let spec = AgentSpec {
            id: AgentId::new("deepseek"),
            display_name: "DeepSeek (API)".into(),
            program: "harness-slint".into(),
            args: vec![crate::helpers::FLAG_API_AGENT.into()],
            env: Vec::new(),
            install_hint: String::new(),
            transport: Transport::Acp,
            dialect: ErrorDialect::Generic,
        };
        let launch = spec.code_mode_launch().unwrap();
        assert_eq!(
            launch.env,
            [(ENV_HARNESS_CODE_MODE.to_owned(), "1".to_owned())]
        );
    }
}
