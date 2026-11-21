use super::*;

#[test]
fn allowlist_blocks_dangerous_methods() {
    for method in [
        "shutdown",
        "import_scan",
        "archive_sessions",
        "create_session",
    ] {
        assert!(!is_allowed(method), "{method} must not be reachable");
    }
    for method in ALLOWED {
        assert!(is_allowed(method));
    }
}
