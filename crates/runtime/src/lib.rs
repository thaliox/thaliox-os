//! # THALIOX runtime (L2)
//!
//! The agent **execution unit** and its lifecycle (MASTER_PLAN §1.4):
//! `born → live → fork → merge → migrate → heal → die`. Scheduling is driven by
//! the [`AttentionBudget`] (TAM §4); per F10 the
//! policy is *learnable*, so this crate fixes the **interface** (telemetry in,
//! next-agent + quota out), not the policy. [`Checkpoint`]s (TAM §6) underpin
//! snapshot / migrate / merge / self-heal.
//!
//! The [`Agent`] is the live unit that runs the TAM contract: every
//! [`act`](Agent::act) is capability-checked (INV-2), budget-charged (INV-1),
//! and audited (INV-4).

pub mod agent;
/// M3 cluster primitives — nodes & migration (RFC-0005 §2–3).
pub mod cluster;
/// M5 learned control plane — the L4 governor (RFC-0007). M5a: the heuristic
/// control loop (observe → policy → actuate through M1–M4 mechanisms).
pub mod control;
/// RFC-0003 §5 falsification gate for the MELD dataflow pillar
/// (E4 dataflow-scheduled forward pass).
pub mod experiment;
/// M2/F3 Firecracker microVM launch target (RFC-0004). Feature-gated.
#[cfg(feature = "firecracker")]
pub mod firecracker;
/// M5c+M5d — the learned policy π_θ, its training simulator, and the
/// falsification gate E5 (RFC-0007 §4–5): learning gated by strict dominance
/// over the baseline; the budget knob and the self-update verdict are learned.
pub mod learn;
/// M2 packaging & one-click deployment (software target; Firecracker later).
pub mod package;
/// M3 supervisor — health & self-healing takeover (RFC-0005 §5).
pub mod supervisor;
/// M2 self-update with generational rollback.
pub mod update;
/// Host ↔ in-VM control protocol (RFC-0004 §4) — shared with the guest-runner.
pub mod vmproto;

pub use agent::{Action, Agent, Outcome};
pub use cluster::{MigrateError, Node, NodeId, migrate};
pub use control::{
    Actuation, AgentObs, Cluster, ClusterState, ControlPlane, Decision, Disposition, GovDecision,
    GovReport, Governor, HeuristicPolicy, Mode, NodeObs, Policy, StateVector, StepReport,
};
#[cfg(feature = "firecracker")]
pub use firecracker::{FcError, FirecrackerConfig, FirecrackerDeploy, MicroVm};
pub use learn::{
    CONTROL_OVERHEAD, EvalReport, GateReport, LearnedPolicy, MAX_GRANT, Ramp, Scenario, SimOutcome,
    e5, evaluate, falsification_gate, held_out_suite, reward, simulate, train, training_suite,
};
pub use package::{DeployEnv, DeployTarget, LocalDeploy, Manifest, Package, PackageError};
pub use supervisor::{HealError, Health, Supervisor};
pub use update::{
    CheckpointHistory, GenStatus, Generation, UpdateError, UpdateOutcome, conclude_update,
};

use serde::{Deserialize, Serialize};
use thaliox_core::{AgentId, AttentionBudget};

/// Lifecycle phase of an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Born,
    Live,
    Forking,
    Merging,
    Migrating,
    Healing,
    Dead,
}

impl Phase {
    /// Whether a direct `self -> next` transition is legal.
    pub fn can_transition_to(&self, next: Phase) -> bool {
        use Phase::*;
        matches!(
            (self, next),
            (Born, Live)
                | (Live, Forking)
                | (Live, Merging)
                | (Live, Migrating)
                | (Live, Healing)
                | (Live, Dead)
                | (Forking, Live)
                | (Merging, Live)
                | (Migrating, Live)
                | (Healing, Live)
        )
    }
}

/// An agent's complete recoverable state (TAM §6): identity + budget + caps +
/// memory pointers + session cursor. The basis of migrate / merge / self-heal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub agent: AgentId,
    pub budget: AttentionBudget,
    /// Opaque, versioned snapshot blob (memory pointers + session cursor + caps).
    pub state: Vec<u8>,
}

/// A scheduler picks the next ready agent to run and the quota it gets. TAM §4.2
/// fixes only this interface — the policy may be replaced by a learned one (F10).
pub trait Scheduler {
    /// Choose the next agent to receive attention, and its budget slice.
    fn next(&self, ready: &[AgentId]) -> Option<(AgentId, AttentionBudget)>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_transitions() {
        assert!(Phase::Born.can_transition_to(Phase::Live));
        assert!(Phase::Live.can_transition_to(Phase::Migrating));
        assert!(Phase::Migrating.can_transition_to(Phase::Live));
        assert!(!Phase::Born.can_transition_to(Phase::Dead));
        assert!(!Phase::Dead.can_transition_to(Phase::Live));
    }
}
