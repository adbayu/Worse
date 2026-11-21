use super::*;

fn tool(function: &str, description: &str) -> Listed {
    Listed {
        function: function.into(),
        description: description.into(),
        definition: Value::Null,
    }
}

#[test]
fn rank_prefers_more_matching_words() {
    let tools = [
        tool("mcp__d__application-deploy", "Deploy an application"),
        tool(
            "mcp__d__application-readLogs",
            "Read deploy logs of an application",
        ),
        tool("mcp__d__backup-create", "Create a backup"),
    ];
    let picked = rank(&tools, &["deploy", "logs"], 15);
    assert_eq!(picked.len(), 2);
    assert_eq!(picked[0].function, "mcp__d__application-readLogs");
    assert_eq!(rank(&tools, &[], 15).len(), 3);
}

#[test]
fn deferred_servers_get_the_load_tool() {
    let set = McpSet::new(&[
        json!({"name": "harness-memory"}),
        json!({"name": "dokploy-mcp"}),
    ]);
    assert_eq!(set.deferred(), vec!["dokploy-mcp"]);
    assert!(McpSet::handles(LOAD_TOOL));
    assert!(McpSet::handles("mcp__dokploy-mcp__x"));
    assert!(!McpSet::handles("read_file"));
    assert_eq!(first_sentence("Deploys it. Then more."), "Deploys it.");
}
