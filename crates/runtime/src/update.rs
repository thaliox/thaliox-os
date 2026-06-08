//! # Self-update with rollback (M2)
//!
//! The third leg of M2 (MASTER_PLAN §6): *self-update rollback*. A self-update
//! is risky — a new version may regress. The safety net is generational
//! snapshots: keep a versioned history of [`Checkpoint`]s, **stage** the
//! post-update state as a *candidate*, health-check it, then either **promote**
//! it (the new known-good baseline) or **roll back** to the last committed
//! generation by [`restore`](crate::Agent::restore)-ing it.
//!
//! Following the crate's "fix the interface, not the policy" stance (TAM §4.2),
//! this module supplies the **mechanism** ([`CheckpointHistory`]) plus a thin
//! [`conclude_update`] helper; the orchestration (when to update, what counts as
//! healthy) belongs to the caller.
//!
//! Invariant: after [`init`](CheckpointHistory::init) the history *always*
//! retains at least one committed generation, so there is always something to
//! roll back to.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use thaliox_cognition::LlmProvider;
use thaliox_core::{SemanticSpace, TamError};

use crate::{Agent, Checkpoint};

/// Whether a generation is the trusted baseline or an unproven candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenStatus {
    /// Verified good — a valid rollback target.
    Committed,
    /// Staged post-update state, not yet proven.
    Candidate,
}

/// One versioned snapshot in the self-update history.
#[derive(Debug, Clone)]
pub struct Generation {
    /// Monotonic generation number (0 = the initial baseline).
    pub number: u64,
    pub status: GenStatus,
    pub checkpoint: Checkpoint,
}

/// Why a history operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// `promote` was called with no candidate staged.
    NoCandidate,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::NoCandidate => write!(f, "no candidate generation to promote"),
        }
    }
}

impl Error for UpdateError {}

/// The result of concluding a self-update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// The candidate passed and is now the committed baseline.
    Committed { generation: u64 },
    /// The candidate failed; the agent was restored to `to_gen`.
    RolledBack { to_gen: u64, reason: String },
}

/// An append-only, generational history of checkpoints supporting rollback.
#[derive(Debug, Clone)]
pub struct CheckpointHistory {
    gens: Vec<Generation>,
    next_gen: u64,
}

impl CheckpointHistory {
    /// Start the history from a known-good `baseline` (generation 0, committed).
    pub fn init(baseline: Checkpoint) -> Self {
        Self {
            gens: vec![Generation {
                number: 0,
                status: GenStatus::Committed,
                checkpoint: baseline,
            }],
            next_gen: 1,
        }
    }

    /// All generations, oldest first.
    pub fn generations(&self) -> &[Generation] {
        &self.gens
    }

    /// The newest generation's number (candidate or committed).
    pub fn current_gen(&self) -> u64 {
        self.gens.last().map(|g| g.number).unwrap_or(0)
    }

    /// The most recent committed (known-good) checkpoint — the rollback target.
    pub fn last_good(&self) -> &Checkpoint {
        self.gens
            .iter()
            .rev()
            .find(|g| g.status == GenStatus::Committed)
            .map(|g| &g.checkpoint)
            .expect("history always retains a committed baseline")
    }

    /// Stage a candidate (post-update) snapshot. Returns its generation number.
    pub fn stage(&mut self, checkpoint: Checkpoint) -> u64 {
        let number = self.next_gen;
        self.next_gen += 1;
        self.gens.push(Generation {
            number,
            status: GenStatus::Candidate,
            checkpoint,
        });
        number
    }

    /// Promote the latest candidate to committed (the update verified good).
    pub fn promote(&mut self) -> Result<u64, UpdateError> {
        match self.gens.last_mut() {
            Some(g) if g.status == GenStatus::Candidate => {
                g.status = GenStatus::Committed;
                Ok(g.number)
            }
            _ => Err(UpdateError::NoCandidate),
        }
    }

    /// Discard all trailing candidates. Returns the generation rolled back to —
    /// always a committed baseline (the invariant guarantees one survives).
    pub fn rollback(&mut self) -> u64 {
        while matches!(self.gens.last(), Some(g) if g.status == GenStatus::Candidate) {
            self.gens.pop();
        }
        self.current_gen()
    }
}

/// Conclude a self-update. The caller has already mutated `agent` and
/// [`stage`](CheckpointHistory::stage)d the candidate; `healthy` is its verdict.
///
/// - healthy ⇒ promote the candidate; `agent` is left as-is (the new baseline).
/// - unhealthy ⇒ roll back and **restore** `agent` from the last good
///   generation, rebinding `memory` + `mind` (re-attach tools/verifier after).
pub fn conclude_update(
    agent: &mut Agent,
    history: &mut CheckpointHistory,
    memory: Arc<dyn SemanticSpace>,
    mind: Arc<dyn LlmProvider>,
    healthy: bool,
    reason: impl Into<String>,
) -> Result<UpdateOutcome, TamError> {
    if healthy {
        let generation = history
            .promote()
            .map_err(|e| TamError::Invalid(e.to_string()))?;
        Ok(UpdateOutcome::Committed { generation })
    } else {
        let to_gen = history.rollback();
        *agent = Agent::restore(history.last_good(), memory, mind)?;
        Ok(UpdateOutcome::RolledBack {
            to_gen,
            reason: reason.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use thaliox_cognition::MockProvider;
    use thaliox_core::{AgentId, AttentionBudget};
    use thaliox_memory::InMemorySpace;

    use super::*;
    use crate::agent::Action;

    fn live_agent(budget: u64) -> Agent {
        let mut a = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(budget, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        );
        a.start().unwrap();
        a
    }

    fn env() -> (Arc<dyn SemanticSpace>, Arc<dyn LlmProvider>) {
        (
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        )
    }

    #[tokio::test]
    async fn failed_update_rolls_back_to_baseline() {
        let mut a = live_agent(100);
        let mut hist = CheckpointHistory::init(a.checkpoint()); // gen0: budget 100, audit 0

        // A "bad update": work we want to undo. Think reconciles to the mock's
        // real token count (5), so the declared cost is just a reservation.
        a.act(Action::Think {
            prompt: "regress".into(),
            cost: 60,
        })
        .await
        .unwrap();
        hist.stage(a.checkpoint()); // gen1 candidate: budget 95, audit 1
        assert_eq!(a.remaining_budget(), 95);

        // Health check fails → roll back.
        let (mem, mind) = env();
        let out = conclude_update(&mut a, &mut hist, mem, mind, false, "v2 regressed").unwrap();

        assert_eq!(
            out,
            UpdateOutcome::RolledBack {
                to_gen: 0,
                reason: "v2 regressed".into()
            }
        );
        // Agent restored to the baseline: budget and audit are back.
        assert_eq!(a.remaining_budget(), 100);
        assert_eq!(a.audit().len(), 0);
        // The candidate is gone; only the committed baseline remains.
        assert_eq!(hist.generations().len(), 1);
        assert_eq!(hist.current_gen(), 0);
    }

    #[tokio::test]
    async fn healthy_update_promotes_new_baseline() {
        let mut a = live_agent(100);
        let mut hist = CheckpointHistory::init(a.checkpoint());

        a.act(Action::Think {
            prompt: "improve".into(),
            cost: 30,
        })
        .await
        .unwrap();
        hist.stage(a.checkpoint()); // gen1 candidate: budget 95 (Think reconciled to 5)

        let (mem, mind) = env();
        let out = conclude_update(&mut a, &mut hist, mem, mind, true, "").unwrap();

        assert_eq!(out, UpdateOutcome::Committed { generation: 1 });
        // No rollback: the agent keeps the updated state.
        assert_eq!(a.remaining_budget(), 95);
        // The new baseline is generation 1.
        assert_eq!(hist.current_gen(), 1);
        assert_eq!(hist.last_good().state, a.checkpoint().state);
    }

    #[test]
    fn rollback_only_drops_candidates_not_committed_history() {
        let a = live_agent(100);
        let cp = a.checkpoint();
        let mut hist = CheckpointHistory::init(cp.clone());

        hist.stage(cp.clone());
        hist.promote().unwrap(); // gen1 committed
        hist.stage(cp.clone()); // gen2 candidate
        hist.stage(cp); // gen3 candidate

        let to = hist.rollback();
        assert_eq!(to, 1); // back to the last committed, not gen0
        assert_eq!(hist.generations().len(), 2);
    }

    #[test]
    fn promote_without_candidate_errors() {
        let a = live_agent(10);
        let mut hist = CheckpointHistory::init(a.checkpoint());
        assert_eq!(hist.promote(), Err(UpdateError::NoCandidate));
    }
}
