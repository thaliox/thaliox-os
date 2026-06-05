//! Primitive #2: the **attention budget** — scheduling & accounting in *tokens*,
//! replacing the CPU time slice. (TAM §4)

use serde::{Deserialize, Serialize};

use crate::error::TamError;

/// A finite attention budget, metered in **tokens** (the natural unit of AI work:
/// inference tokens plus the token-equivalent of retrieval / communication).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionBudget {
    /// Total token budget granted.
    pub total: u64,
    /// Tokens consumed so far.
    pub spent: u64,
    /// Per-second consumption ceiling (tokens/s), for rate limiting.
    pub rate: u64,
    /// How the budget refills.
    pub refill: RefillPolicy,
}

/// How an exhausted budget is replenished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RefillPolicy {
    /// Never refills.
    None,
    /// Refills `per_sec` tokens each second.
    Periodic { per_sec: u64 },
    /// Refilled on demand by the scheduler.
    OnDemand,
}

impl AttentionBudget {
    /// A fixed budget that does not refill.
    pub fn new(total: u64, rate: u64) -> Self {
        Self {
            total,
            spent: 0,
            rate,
            refill: RefillPolicy::None,
        }
    }

    /// Tokens still available.
    pub fn remaining(&self) -> u64 {
        self.total.saturating_sub(self.spent)
    }

    /// **INV-1 (budget conservation)** — charge `cost` tokens before a call
    /// executes, or reject with [`TamError::BudgetExceeded`] if the balance is
    /// insufficient. The charge is all-or-nothing.
    pub fn charge(&mut self, cost: u64) -> Result<(), TamError> {
        let have = self.remaining();
        if cost > have {
            return Err(TamError::BudgetExceeded { need: cost, have });
        }
        self.spent += cost;
        Ok(())
    }

    /// **INV-1 reconciliation** — settle a prior [`charge`](Self::charge)d
    /// reservation against the *actual* cost once it is known (e.g. a real
    /// completion's token usage). If `actual` exceeds the reservation the
    /// surplus is recorded — the tokens were genuinely spent, so the balance may
    /// reach zero or overdraw; if it is less, the difference is refunded.
    pub fn settle(&mut self, reserved: u64, actual: u64) {
        if actual >= reserved {
            self.spent = self.spent.saturating_add(actual - reserved);
        } else {
            self.spent = self.spent.saturating_sub(reserved - actual);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_conserves_and_rejects() {
        let mut b = AttentionBudget::new(100, 10);
        assert_eq!(b.remaining(), 100);
        b.charge(60).unwrap();
        assert_eq!(b.remaining(), 40);
        // INV-1: overspend is rejected and leaves the balance untouched.
        assert!(matches!(
            b.charge(50),
            Err(TamError::BudgetExceeded { need: 50, have: 40 })
        ));
        assert_eq!(b.remaining(), 40);
    }

    #[test]
    fn settle_reconciles_to_actual() {
        // Reserve 30, actual 45 → really spend 45 (overshoot recorded).
        let mut over = AttentionBudget::new(100, 10);
        over.charge(30).unwrap();
        over.settle(30, 45);
        assert_eq!(over.remaining(), 55);

        // Reserve 30, actual 10 → refund the 20 surplus.
        let mut under = AttentionBudget::new(100, 10);
        under.charge(30).unwrap();
        under.settle(30, 10);
        assert_eq!(under.remaining(), 90);
    }
}
