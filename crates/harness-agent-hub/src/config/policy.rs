use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use harness_agent_config::{AuthProbe, FileExistenceProbe};
use harness_config::Limits;
use harness_core_api::FileSearchSource;

/// Live auto-update switch shared between the UI and the hub.
#[derive(Debug, Clone)]
pub struct AutoUpdatePolicy {
    on: Arc<AtomicBool>,
}

impl Default for AutoUpdatePolicy {
    fn default() -> Self {
        Self {
            on: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl AutoUpdatePolicy {
    #[must_use]
    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn set_on(&self, value: bool) {
        self.on.store(value, Ordering::Relaxed);
    }
}

/// Live fact-check switch shared between the UI and the hub.
#[derive(Debug, Clone)]
pub struct FactCheckPolicy {
    on: Arc<AtomicBool>,
}

impl Default for FactCheckPolicy {
    fn default() -> Self {
        Self {
            on: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl FactCheckPolicy {
    #[must_use]
    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn set_on(&self, value: bool) {
        self.on.store(value, Ordering::Relaxed);
    }
}

/// Live delegation settings shared between the UI and the hub.
#[derive(Debug, Clone)]
pub struct DelegationPolicy {
    limit: Arc<AtomicUsize>,
    nested_limit: Arc<AtomicUsize>,
    nested: Arc<AtomicBool>,
}

impl Default for DelegationPolicy {
    fn default() -> Self {
        Self {
            limit: Arc::new(AtomicUsize::new(Limits::DEFAULT.max_subagents)),
            nested_limit: Arc::new(AtomicUsize::new(Limits::DEFAULT.max_nested_subagents)),
            nested: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl DelegationPolicy {
    /// Direct subagents that may run at once under one top-level session (at least 1); nested ones do not count.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit.load(Ordering::Relaxed).max(1)
    }

    pub fn set_limit(&self, value: usize) {
        self.limit.store(value.max(1), Ordering::Relaxed);
    }

    /// Subagents that one subagent may run at once under itself (its own quota, at least 1).
    #[must_use]
    pub fn nested_limit(&self) -> usize {
        self.nested_limit.load(Ordering::Relaxed).max(1)
    }

    pub fn set_nested_limit(&self, value: usize) {
        self.nested_limit.store(value.max(1), Ordering::Relaxed);
    }

    /// The quota that applies to `parent`'s own subagents.
    #[must_use]
    pub fn limit_for(&self, parent_is_subagent: bool) -> usize {
        if parent_is_subagent {
            self.nested_limit()
        } else {
            self.limit()
        }
    }

    /// Whether a subagent may delegate to subagents of its own.
    #[must_use]
    pub fn nested(&self) -> bool {
        self.nested.load(Ordering::Relaxed)
    }

    pub fn set_nested(&self, value: bool) {
        self.nested.store(value, Ordering::Relaxed);
    }
}

/// Shared [`FileSearchSource`] (`HubConfig` stays `Debug`/`Clone`).
#[derive(Clone)]
pub struct SharedFileSearch(pub Arc<dyn FileSearchSource>);

impl std::fmt::Debug for SharedFileSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedFileSearch")
    }
}

/// Shared [`AuthProbe`] (gates inject a static one).
#[derive(Clone)]
pub struct SharedAuthProbe(pub Arc<dyn AuthProbe>);

impl Default for SharedAuthProbe {
    fn default() -> Self {
        Self(Arc::new(FileExistenceProbe))
    }
}

impl std::fmt::Debug for SharedAuthProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedAuthProbe")
    }
}
