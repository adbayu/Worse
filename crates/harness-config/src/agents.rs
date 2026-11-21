use serde::Deserialize;

const AGENTS_TOML: &str = include_str!("../assets/agents.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Dialect {
    ClaudeCode,
    Codex,
    Agy,
    Generic,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BuiltinAgent {
    pub id: String,
    pub display_name: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub install_hint: String,
    pub dialect: Dialect,
}

#[derive(Deserialize)]
struct AgentsToml {
    agent: Vec<BuiltinAgent>,
}

#[must_use]
pub fn builtin_agents() -> Vec<BuiltinAgent> {
    parse(AGENTS_TOML).unwrap_or_default()
}

fn parse(text: &str) -> Result<Vec<BuiltinAgent>, toml::de::Error> {
    toml::from_str::<AgentsToml>(text).map(|file| file.agent)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_embedded_file_parses_in_failover_order() {
        let agents = parse(AGENTS_TOML).unwrap();
        let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["claude-code", "codex", "agy", "command-code"]);
        assert_eq!(agents[2].args, ["acp"]);
        assert_eq!(agents[3].dialect, Dialect::Generic);
    }
}
