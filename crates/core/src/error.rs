//! Errors named directly by the TAM invariants.

use thiserror::Error;

/// The canonical TAM error set. Each variant maps to a violated invariant.
#[derive(Debug, Error)]
pub enum TamError {
    /// INV-1: a call whose cost exceeds the caller's remaining attention budget.
    #[error("attention budget exceeded: need {need}, have {have}")]
    BudgetExceeded { need: u64, have: u64 },

    /// INV-2: a call lacking a capability that grants the required permission
    /// *and* a scope covering the target.
    #[error("capability denied: {0}")]
    CapabilityDenied(String),

    /// INV-3: a vector message crossed model spaces without explicit, measured
    /// translation. Implicit lossy conversion is forbidden.
    #[error(
        "vector translation required (from '{from}' to '{to}') — implicit lossy conversion is forbidden"
    )]
    TranslationRequired { from: String, to: String },

    /// A referenced object / agent / capability does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// A malformed argument.
    #[error("invalid argument: {0}")]
    Invalid(String),

    /// A cognition / external provider failed (network, HTTP, or bad response).
    #[error("provider error: {0}")]
    Provider(String),
}
