use super::*;

#[test]
fn allowlist_blocks_dangerous_methods() {
    for method in ["shutdown", "import_scan", "archive_sessions"] {
        assert!(!is_allowed(method), "{method} must not be reachable");
    }
    for method in ALLOWED {
        assert!(is_allowed(method));
    }
}

#[test]
fn create_paths_compare_loosely() {
    assert_eq!(create::path_key("D:/Worse/"), create::path_key(r"d:\worse"));
    assert_ne!(
        create::path_key(r"D:\Worse2"),
        create::path_key(r"D:\Worse")
    );
    assert_eq!(create::path_key("  "), "");
}
