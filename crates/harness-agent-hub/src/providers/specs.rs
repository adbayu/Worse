use super::{
    load, state_dir, AgentId, AgentSpec, ErrorDialect, HubConfig, ProviderEntry, Transport,
    COMMAND_CODE, COMMAND_CODE_PREFIX, ENV_API_KEY, ENV_BASE_URL, ENV_COMMAND_CODE_KEY, ENV_LABEL,
    ENV_MODEL, ENV_STATE,
};

/// Agent of one Command Code key: the built-in Command Code agent with the key in its environment.
fn command_code_spec(config: &HubConfig, entry: &ProviderEntry) -> AgentSpec {
    let mut spec = config
        .agents
        .iter()
        .find(|a| a.id.as_str() == COMMAND_CODE)
        .cloned()
        .or_else(|| {
            crate::agents::builtin_agents()
                .into_iter()
                .find(|a| a.id.as_str() == COMMAND_CODE)
        })
        .unwrap_or_else(|| AgentSpec {
            id: AgentId::new(COMMAND_CODE),
            display_name: "Command Code".into(),
            program: "command-code".into(),
            args: vec!["acp".into()],
            env: Vec::new(),
            install_hint: "install Command Code: npm i -g command-code".into(),
            transport: Transport::Acp,
            dialect: ErrorDialect::Generic,
        });
    spec.id = AgentId::new(entry.id.clone());
    spec.display_name = format!("Command Code · {}", entry.label);
    spec.env.retain(|(k, _)| k != ENV_COMMAND_CODE_KEY);
    spec.env
        .push((ENV_COMMAND_CODE_KEY.into(), entry.api_key.clone()));
    spec
}

fn spec_of(config: &HubConfig, entry: &ProviderEntry) -> AgentSpec {
    if entry.is_command_code() {
        return command_code_spec(config, entry);
    }
    let state = state_dir(&config.home, &entry.id);
    // §9.9：程序与参数只由 HelperLaunch 决定（产品里是 harness-slint --api-agent）。
    let helper = crate::helpers::Helper::ApiAgent;
    AgentSpec {
        id: AgentId::new(entry.id.clone()),
        display_name: format!("{} (API)", entry.label),
        program: config
            .helpers
            .program(helper)
            .to_string_lossy()
            .into_owned(),
        args: config.helpers.args(helper),
        env: vec![
            (ENV_BASE_URL.into(), entry.base_url.clone()),
            (ENV_MODEL.into(), entry.model.clone()),
            (ENV_API_KEY.into(), entry.api_key.clone()),
            (ENV_LABEL.into(), entry.label.clone()),
            (ENV_STATE.into(), state.to_string_lossy().into_owned()),
        ],
        install_hint:
            "the Harness executable that runs the API agent is missing; reinstall Harness".into(),
        transport: Transport::Acp,
        dialect: ErrorDialect::Generic,
    }
}

/// Adds the providers of `<home>/providers.json` to the agents, at the end of the default order. A broken file
/// is logged (without content) and skipped: the other agents must still start.
pub(crate) async fn apply(config: &mut HubConfig) {
    let file = match load(&config.home).await {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(%error, "providers.json ignored");
            return;
        }
    };
    for entry in &file.providers {
        let id = AgentId::new(entry.id.clone());
        if config.agents.iter().any(|a| a.id == id) {
            continue;
        }
        let spec = spec_of(config, entry);
        if entry.is_command_code() {
            // Next to the other Command Code agents, so failover tries the next key before another agent.
            let position = config
                .default_fallback_order
                .iter()
                .rposition(|a| is_command_code_agent(a.as_str()))
                .map_or(config.default_fallback_order.len(), |i| i + 1);
            config.default_fallback_order.insert(position, id);
        } else {
            config.default_fallback_order.push(id);
        }
        config.agents.push(spec);
    }
    config.key_pools = pools(&file.providers);
    if file
        .rotation
        .as_deref()
        .is_some_and(|r| r.eq_ignore_ascii_case("off"))
    {
        config.rotate_keys = false;
    }
}

pub(super) fn is_command_code_agent(id: &str) -> bool {
    id == COMMAND_CODE || id.starts_with(COMMAND_CODE_PREFIX)
}

/// Key pools of the entries: all Command Code keys (with the built-in agent as the name of the pool, so a
/// session asked for "Command Code" rotates over the keys), and every group of two or more providers that
/// share endpoint and model.
pub(crate) fn pools(entries: &[ProviderEntry]) -> Vec<Vec<AgentId>> {
    let mut out = Vec::new();
    let keys: Vec<AgentId> = entries
        .iter()
        .filter(|e| e.is_command_code() && !e.api_key.trim().is_empty())
        .map(|e| AgentId::new(e.id.clone()))
        .collect();
    if !keys.is_empty() {
        out.push(keys);
    }
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for entry in entries.iter().filter(|e| !e.is_command_code()) {
        let key = (entry.base_url.as_str(), entry.model.as_str());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let group: Vec<AgentId> = entries
            .iter()
            .filter(|e| !e.is_command_code() && (e.base_url.as_str(), e.model.as_str()) == key)
            .map(|e| AgentId::new(e.id.clone()))
            .collect();
        if group.len() > 1 {
            out.push(group);
        }
    }
    out
}
