use std::path::PathBuf;

pub const HOME_ENV: &str = "HARNESS_HOME";
pub const HOME_DIR_NAME: &str = ".harness";

#[must_use]
pub fn harness_home() -> Option<PathBuf> {
    match std::env::var_os(HOME_ENV) {
        Some(home) if !home.is_empty() => Some(PathBuf::from(home)),
        _ => user_home().map(|home| home.join(HOME_DIR_NAME)),
    }
}

#[must_use]
pub fn user_home() -> Option<PathBuf> {
    let keys: &[&str] = if cfg!(windows) {
        &["USERPROFILE", "HOME"]
    } else {
        &["HOME"]
    };
    keys.iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .map(PathBuf::from)
}
