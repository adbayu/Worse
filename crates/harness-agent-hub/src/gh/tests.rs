use super::release::tag_of;

#[test]
fn tag_comes_from_the_latest_release_redirect() {
    assert_eq!(
        tag_of("https://github.com/cli/cli/releases/tag/v2.105.0").as_deref(),
        Some("v2.105.0")
    );
    assert_eq!(tag_of("https://github.com/cli/cli/releases"), None);
    assert_eq!(tag_of("https://github.com/cli/cli/releases/tag/"), None);
}
