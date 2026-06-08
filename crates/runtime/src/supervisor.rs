//! # Supervisor — health & self-healing (M3 / RFC-0005 §5)
//!
//! Holds a registry `AgentId → (node, last good checkpoint, health)` and turns a
//! failure signal into a takeover: detect (missed heartbeats) → restore the last
//! good [`Checkpoint`] on a healthy [`Node`] → flip the registry. A recovered
//! split-brain instance is **reconciled** via the CRDT [`Checkpoint::merge`]
//! (M3a) rather than run as a second authority.
//!
//! *Mechanism, not policy* (TAM §4.2): the supervisor exposes `tick`/`health`/
//! `self_heal`/`reconcile`; *when* to heal is the caller's (later, M5's) policy.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use thaliox_core::AgentId;

use crate::{Agent, Checkpoint, DeployEnv, Node, NodeId};

/// An agent's liveness as seen by the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    /// Missed at least `miss_threshold` consecutive heartbeats — assume down.
    Suspected,
}

struct Record {
    node: NodeId,
    last_good: Checkpoint,
    misses: u32,
}

/// Why a heal/reconcile failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealError {
    NotRegistered(AgentId),
    Restore(String),
    Merge(String),
}

impl fmt::Display for HealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HealError::NotRegistered(id) => write!(f, "agent {id} is not registered"),
            HealError::Restore(e) => write!(f, "restore failed: {e}"),
            HealError::Merge(e) => write!(f, "reconcile merge failed: {e}"),
        }
    }
}

impl Error for HealError {}

/// Tracks where agents run, their last good checkpoint, and their health.
pub struct Supervisor {
    agents: HashMap<AgentId, Record>,
    miss_threshold: u32,
}

impl Supervisor {
    /// `miss_threshold` consecutive missed heartbeats ⇒ `Suspected`.
    pub fn new(miss_threshold: u32) -> Self {
        Self {
            agents: HashMap::new(),
            miss_threshold: miss_threshold.max(1),
        }
    }

    /// Register (or update) where an agent runs and its latest good checkpoint.
    /// The supervisor pulls these periodically — over vsock in a VM (F3), or
    /// directly in-process.
    pub fn observe(&mut self, id: &AgentId, node: NodeId, checkpoint: Checkpoint) {
        match self.agents.get_mut(id) {
            Some(r) => {
                r.node = node;
                r.last_good = checkpoint;
            }
            None => {
                self.agents.insert(
                    id.clone(),
                    Record {
                        node,
                        last_good: checkpoint,
                        misses: 0,
                    },
                );
            }
        }
    }

    /// The agent reported alive this cycle (resets its miss counter).
    pub fn heartbeat(&mut self, id: &AgentId) {
        if let Some(r) = self.agents.get_mut(id) {
            r.misses = 0;
        }
    }

    /// Advance one supervision cycle: every agent misses a beat unless it
    /// heartbeated since the last tick. Returns the agents that *just* crossed
    /// into `Suspected`.
    pub fn tick(&mut self) -> Vec<AgentId> {
        let threshold = self.miss_threshold;
        let mut newly_suspected = Vec::new();
        for (id, r) in self.agents.iter_mut() {
            let was = r.misses >= threshold;
            r.misses += 1;
            if !was && r.misses >= threshold {
                newly_suspected.push(id.clone());
            }
        }
        newly_suspected
    }

    pub fn health(&self, id: &AgentId) -> Option<Health> {
        self.agents.get(id).map(|r| {
            if r.misses >= self.miss_threshold {
                Health::Suspected
            } else {
                Health::Healthy
            }
        })
    }

    pub fn node_of(&self, id: &AgentId) -> Option<&NodeId> {
        self.agents.get(id).map(|r| &r.node)
    }

    /// Self-heal: restore the agent's last good checkpoint onto `healthy` and
    /// flip the registry to it (RFC-0005 §5). The registry pointing to the new
    /// node *is* the fence — a returning old instance must [`reconcile`], not
    /// resume as a second authority.
    ///
    /// [`reconcile`]: Supervisor::reconcile
    pub fn self_heal(
        &mut self,
        id: &AgentId,
        healthy: &mut Node,
        env: DeployEnv,
    ) -> Result<(), HealError> {
        let last_good = self
            .agents
            .get(id)
            .ok_or_else(|| HealError::NotRegistered(id.clone()))?
            .last_good
            .clone();

        let mut agent = Agent::restore(&last_good, env.memory, env.mind)
            .map_err(|e| HealError::Restore(e.to_string()))?;
        for tool in env.tools {
            agent = agent.with_tool(tool);
        }
        if let Some(v) = env.verifier {
            agent = agent.with_verifier(v);
        }
        healthy.host(agent);

        let r = self.agents.get_mut(id).expect("checked above");
        r.node = healthy.id().clone();
        r.misses = 0;
        Ok(())
    }

    /// Reconcile a returning instance's diverged checkpoint into the
    /// authoritative state via CRDT merge (M3a) — so a recovered split-brain is
    /// merged, never run twice. Updates and returns the new last-good checkpoint.
    pub fn reconcile(
        &mut self,
        id: &AgentId,
        returning: &Checkpoint,
    ) -> Result<Checkpoint, HealError> {
        let r = self
            .agents
            .get_mut(id)
            .ok_or_else(|| HealError::NotRegistered(id.clone()))?;
        let merged = r
            .last_good
            .merge(returning)
            .map_err(|e| HealError::Merge(e.to_string()))?;
        r.last_good = merged.clone();
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use thaliox_cognition::MockProvider;
    use thaliox_core::{AgentId, AttentionBudget};
    use thaliox_memory::InMemorySpace;

    use super::*;
    use crate::Action;

    fn fresh_env() -> DeployEnv {
        DeployEnv {
            memory: Arc::new(InMemorySpace::new()),
            mind: Arc::new(MockProvider::new("ok", 5)),
            tools: vec![],
            verifier: None,
        }
    }

    async fn agent_after_work(id: &str, budget: u64, beats: usize) -> Agent {
        let mut a = Agent::new(
            AgentId::new(id),
            AttentionBudget::new(budget, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        );
        a.start().unwrap();
        for _ in 0..beats {
            a.act(Action::Think {
                prompt: "w".into(),
                cost: 5,
            })
            .await
            .unwrap();
        }
        a
    }

    #[tokio::test]
    async fn detects_missed_heartbeats() {
        let mut sup = Supervisor::new(3);
        let a = agent_after_work("a1", 100, 0).await;
        let id = a.id().clone();
        sup.observe(&id, NodeId::new("A"), a.checkpoint());

        // Healthy while it heartbeats.
        for _ in 0..5 {
            sup.heartbeat(&id);
            assert!(sup.tick().is_empty());
            assert_eq!(sup.health(&id), Some(Health::Healthy));
        }
        // Reset to a clean baseline, then stop beating — Suspected at the 3rd miss.
        sup.heartbeat(&id);
        assert!(sup.tick().is_empty()); // miss 1
        assert!(sup.tick().is_empty()); // miss 2
        assert_eq!(sup.tick(), vec![id.clone()]); // miss 3 → newly suspected
        assert_eq!(sup.health(&id), Some(Health::Suspected));
    }

    #[tokio::test]
    async fn self_heal_restores_on_a_healthy_node() {
        let mut sup = Supervisor::new(2);
        let a = agent_after_work("a1", 100, 1).await; // budget 95, audit 1
        let id = a.id().clone();
        sup.observe(&id, NodeId::new("A"), a.checkpoint());

        // Node A goes silent → suspected.
        sup.tick();
        sup.tick();
        assert_eq!(sup.health(&id), Some(Health::Suspected));

        // Heal onto node B from the last good checkpoint.
        let mut node_b = Node::new("B");
        sup.self_heal(&id, &mut node_b, fresh_env()).unwrap();

        assert_eq!(sup.node_of(&id), Some(&NodeId::new("B")));
        assert_eq!(sup.health(&id), Some(Health::Healthy)); // misses reset
        let healed = node_b.agent(&id).unwrap();
        assert_eq!(healed.remaining_budget(), 95); // state restored
        assert_eq!(healed.audit().len(), 1);
    }

    #[tokio::test]
    async fn reconcile_merges_a_returning_split_brain() {
        let mut sup = Supervisor::new(2);
        // authoritative last-good: spent 5, one audit record.
        let primary = agent_after_work("a1", 100, 1).await; // budget 95
        let id = primary.id().clone();
        sup.observe(&id, NodeId::new("A"), primary.checkpoint());

        // A returning instance that diverged: did more work (spent 10).
        let returned = agent_after_work("a1", 100, 2).await; // budget 90
        let merged = sup.reconcile(&id, &returned.checkpoint()).unwrap();

        // CRDT merge: spent is the join (max) → 10 spent → remaining 90.
        assert_eq!(merged.budget.spent, 10);
    }
}
