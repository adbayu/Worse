use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Limits {
    pub agent_idle_secs: u64,
    pub shutdown_grace_secs: u64,
    pub event_capacity: usize,
    pub schedule_min_interval_secs: u32,
    pub max_subagents: usize,
    pub max_nested_subagents: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Limits {
    pub const DEFAULT: Self = Self {
        agent_idle_secs: 300,
        shutdown_grace_secs: 3,
        event_capacity: 4096,
        schedule_min_interval_secs: 300,
        max_subagents: 4,
        max_nested_subagents: 4,
    };

    #[must_use]
    pub const fn agent_idle(&self) -> Duration {
        Duration::from_secs(self.agent_idle_secs)
    }

    #[must_use]
    pub const fn shutdown_grace(&self) -> Duration {
        Duration::from_secs(self.shutdown_grace_secs)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn default_and_const_agree() {
        assert_eq!(Limits::default(), Limits::DEFAULT);
    }

    #[test]
    fn partial_override_keeps_the_rest() {
        let limits: Limits = toml::from_str("eventCapacity = 10").unwrap();
        assert_eq!(limits.event_capacity, 10);
        assert_eq!(limits.agent_idle_secs, 300);
    }
}
