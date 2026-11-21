//! Agent form round-trip tests for the config page.

mod mcp_agents_tests {
    use super::super::entry::*;
    use super::super::{EntryBody, McpServer};

    fn server() -> EntryBody {
        EntryBody::Mcp(McpServer {
            name: "ssh".into(),
            command: Some("node".into()),
            agents: vec!["claude-code".into(), "codex".into()],
            ..McpServer::default()
        })
    }

    fn agents_of(body: &EntryBody) -> Vec<String> {
        match body {
            EntryBody::Mcp(s) => s.agents.clone(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn overlay_form_round_trips_agents() {
        let mut fields = fields_of(&server(), true);
        let agents = fields.iter_mut().find(|(k, _)| k == "agents");
        assert_eq!(
            agents.as_ref().map(|(_, v)| v.as_str()),
            Some("claude-code, codex")
        );
        if let Some((_, v)) = agents {
            *v = " codex ,\napi-deepseek, ".into();
        }
        let body = body_from(server(), &fields, true);
        assert_eq!(
            agents_of(&body),
            vec!["codex".to_string(), "api-deepseek".to_string()]
        );
    }

    #[test]
    fn cli_targets_have_no_agents_field_and_keep_the_value() {
        let fields = fields_of(&server(), false);
        assert!(fields.iter().all(|(k, _)| k != "agents"));
        let body = body_from(server(), &fields, false);
        assert_eq!(
            agents_of(&body),
            vec!["claude-code".to_string(), "codex".to_string()]
        );
    }
}
