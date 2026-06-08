//! # Cluster primitives — nodes & migration (M3 / RFC-0005 §2–3)
//!
//! A [`Node`] hosts running agents; [`migrate`] moves one between nodes by its
//! **portable `Package`** — capture on the source, ship the bytes, deploy on the
//! target, then drain the source (stop-and-copy cutover). The wire step goes
//! through `Package::to_bytes`/`from_bytes`, so the same flow works in-process
//! (here) or across hosts (where the bytes cross vsock / the network, RFC-0004).

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use thaliox_core::AgentId;

use crate::{Agent, DeployEnv, DeployTarget, LocalDeploy, Manifest, Package, PackageError};

/// A node identity in the cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn new(id: impl Into<String>) -> Self {
        NodeId(id.into())
    }
}

/// A host that runs agents. Minimal: a named bag keyed by `AgentId`.
pub struct Node {
    id: NodeId,
    agents: HashMap<AgentId, Agent>,
}

impl Node {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: NodeId::new(id),
            agents: HashMap::new(),
        }
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }

    /// Place a running agent on this node (keyed by its id).
    pub fn host(&mut self, agent: Agent) {
        self.agents.insert(agent.id().clone(), agent);
    }

    pub fn agent(&self, id: &AgentId) -> Option<&Agent> {
        self.agents.get(id)
    }

    pub fn agent_mut(&mut self, id: &AgentId) -> Option<&mut Agent> {
        self.agents.get_mut(id)
    }

    /// Remove and return an agent (drain on cutover).
    pub fn take(&mut self, id: &AgentId) -> Option<Agent> {
        self.agents.remove(id)
    }

    pub fn hosts(&self, id: &AgentId) -> bool {
        self.agents.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.agents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }
}

/// Why a migration failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateError {
    /// The source node is not hosting that agent.
    NotHosted(AgentId),
    /// The target rejected the package (validation / restore).
    Deploy(PackageError),
}

impl fmt::Display for MigrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MigrateError::NotHosted(id) => write!(f, "agent {id} is not hosted on the source node"),
            MigrateError::Deploy(e) => write!(f, "target rejected migration: {e}"),
        }
    }
}

impl Error for MigrateError {}

/// Migrate agent `id` from `src` to `dst` (RFC-0005 §3, stop-and-copy):
/// capture a `Package` on the source → ship the bytes → deploy on the target
/// (rebinding `dst_env`) → drain the source. The agent's state survives the move;
/// on return, `dst` hosts it and `src` no longer does.
pub fn migrate(
    src: &mut Node,
    dst: &mut Node,
    id: &AgentId,
    dst_env: DeployEnv,
) -> Result<(), MigrateError> {
    // Capture — an empty manifest accepts any target environment; the receiving
    // node supplies a compatible memory/mind (external store is addressable).
    let pkg = {
        let agent = src
            .agent(id)
            .ok_or_else(|| MigrateError::NotHosted(id.clone()))?;
        Package::pack(agent, Manifest::new(id.clone()))
    };

    // Transfer — over the wire as bytes (in-process here; vsock/net across hosts).
    let wire = pkg.to_bytes();
    let arrived = Package::from_bytes(&wire).map_err(MigrateError::Deploy)?;

    // Restore on the target, then cut over (drain the source).
    let restored = LocalDeploy
        .deploy(&arrived, dst_env)
        .map_err(MigrateError::Deploy)?;
    dst.host(restored);
    src.take(id);
    Ok(())
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

    fn live_agent(id: &str, budget: u64) -> Agent {
        let mut a = Agent::new(
            AgentId::new(id),
            AttentionBudget::new(budget, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        );
        a.start().unwrap();
        a
    }

    #[tokio::test]
    async fn migrate_moves_agent_and_preserves_state() {
        let mut a = live_agent("a1", 100);
        a.act(Action::Think {
            prompt: "work".into(),
            cost: 5,
        })
        .await
        .unwrap();
        let budget_before = a.remaining_budget(); // 95 (Think reconciled to mock's 5)
        let audit_before = a.audit().len();

        let mut node_a = Node::new("A");
        let mut node_b = Node::new("B");
        node_a.host(a);

        let id = AgentId::new("a1");
        migrate(&mut node_a, &mut node_b, &id, fresh_env()).unwrap();

        // Cutover: B hosts it, A drained.
        assert!(node_b.hosts(&id));
        assert!(!node_a.hosts(&id));
        assert_eq!(node_a.len(), 0);

        // State survived the move (through Package bytes).
        let moved = node_b.agent(&id).unwrap();
        assert_eq!(moved.remaining_budget(), budget_before);
        assert_eq!(moved.audit().len(), audit_before);
        assert_eq!(moved.phase(), crate::Phase::Live);
    }

    #[test]
    fn migrate_unknown_agent_errors() {
        let mut a = Node::new("A");
        let mut b = Node::new("B");
        let id = AgentId::new("ghost");
        assert_eq!(
            migrate(&mut a, &mut b, &id, fresh_env()),
            Err(MigrateError::NotHosted(id))
        );
    }

    #[tokio::test]
    async fn migrate_is_reversible() {
        let mut node_a = Node::new("A");
        let mut node_b = Node::new("B");
        node_a.host(live_agent("a1", 50));
        let id = AgentId::new("a1");

        migrate(&mut node_a, &mut node_b, &id, fresh_env()).unwrap();
        assert!(node_b.hosts(&id));
        migrate(&mut node_b, &mut node_a, &id, fresh_env()).unwrap();
        assert!(node_a.hosts(&id));
        assert!(node_b.is_empty());
    }
}
