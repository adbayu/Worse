use super::*;

/// A composer choice made before any session exists must survive into the created session instead of the hub
/// picking its default order (Claude Code first).
#[test]
fn create_session_passes_the_composer_agent() {
    let request = create_request(Path::new("C:/w"), false, &"codex".to_string(), None);
    assert_eq!(request.agent, Some(api::AgentId::new("codex")));
    assert_eq!(request.workspace_path, "C:/w");
    assert!(request.worktree.is_none());
}

#[test]
fn create_session_without_a_choice_lets_the_hub_pick() {
    let request = create_request(Path::new("C:/w"), true, &String::new(), None);
    assert_eq!(request.agent, None);
    assert!(request.worktree.is_some());
}

/// “Open folder…”：选中的文件夹作为新会话的工作区，而不是进程启动时的工作区。
#[test]
fn create_session_uses_the_chosen_folder() {
    let request = create_request(
        Path::new("C:/startup"),
        false,
        &"codex".to_string(),
        Some(Path::new("D:/picked/project")),
    );
    assert_eq!(request.workspace_path, "D:/picked/project");
    assert!(request.worktree.is_none());
    assert_eq!(request.agent, Some(api::AgentId::new("codex")));
}
