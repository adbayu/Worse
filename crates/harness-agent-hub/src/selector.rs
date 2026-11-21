use std::sync::Arc;

use harness_core_api::AgentId;

pub struct Selection<'a> {
    pub requested: Option<&'a AgentId>,
    pub sticky: &'a AgentId,
    pub order: &'a [AgentId],
    pub tried: &'a [AgentId],
}

pub trait AgentSelector: Send + Sync {
    fn select(&self, input: &Selection<'_>, usable: &dyn Fn(&AgentId) -> bool) -> Option<AgentId>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StickyThenFallback;

impl AgentSelector for StickyThenFallback {
    fn select(&self, input: &Selection<'_>, usable: &dyn Fn(&AgentId) -> bool) -> Option<AgentId> {
        let fresh = |agent: &AgentId| !input.tried.contains(agent);
        if let Some(agent) = input.requested {
            return fresh(agent).then(|| agent.clone());
        }
        let ok = |agent: &AgentId| fresh(agent) && usable(agent);
        if ok(input.sticky) {
            return Some(input.sticky.clone());
        }
        let order = input.order;
        let start = order
            .iter()
            .position(|a| a == input.sticky)
            .map_or(0, |i| i + 1);
        (0..order.len())
            .map(|offset| &order[(start + offset) % order.len()])
            .find(|agent| ok(agent))
            .cloned()
    }
}

#[derive(Clone)]
pub struct SharedAgentSelector(pub Arc<dyn AgentSelector>);

impl Default for SharedAgentSelector {
    fn default() -> Self {
        Self(Arc::new(StickyThenFallback))
    }
}

impl std::fmt::Debug for SharedAgentSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedAgentSelector")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(names: &[&str]) -> Vec<AgentId> {
        names.iter().map(|n| AgentId::new(*n)).collect()
    }

    #[test]
    fn walks_the_order_after_the_sticky_agent() {
        let order = ids(&["a", "b", "c"]);
        let tried = ids(&["b"]);
        let input = Selection {
            requested: None,
            sticky: &order[1],
            order: &order,
            tried: &tried,
        };
        let pick = StickyThenFallback.select(&input, &|agent| agent.as_str() != "c");
        assert_eq!(pick, Some(AgentId::new("a")));
    }

    #[test]
    fn a_requested_agent_wins_unless_it_already_tried() {
        let order = ids(&["a", "b"]);
        let requested = AgentId::new("b");
        let mut input = Selection {
            requested: Some(&requested),
            sticky: &order[0],
            order: &order,
            tried: &[],
        };
        assert_eq!(
            StickyThenFallback.select(&input, &|_| false),
            Some(requested.clone())
        );
        let tried = ids(&["b"]);
        input.tried = &tried;
        assert_eq!(StickyThenFallback.select(&input, &|_| true), None);
    }
}
