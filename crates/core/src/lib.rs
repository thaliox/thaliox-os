//! # THALIOX core — the THALIOX Abstract Machine (TAM)
//!
//! This crate is the code-level encoding of [RFC-0001](../../../docs/rfcs/0001-abstract-machine.md):
//! an **implementation-independent contract** that the software runtime (H1, on
//! Linux) and a future co-designed silicon (H3) both target — so evolving from
//! prototype to custom hardware is *replacing implementations, not rebuilding*.
//!
//! ## The three primitives
//!
//! AI agents have three first-class primitives; lifting them to machine-level
//! citizens simplifies the whole stack (scheduling, security, communication):
//!
//! 1. [`VectorMessage`] — the unit of **meaning** exchanged between agents (not bytes).
//! 2. [`AttentionBudget`] — the unit of **scheduling & accounting** (tokens, not CPU time slices).
//! 3. [`CapabilityToken`] — the unit of **permission & trust** (not uid/gid).
//!
//! ## The five invariants (every implementation MUST satisfy)
//!
//! - **INV-1 (budget conservation)** — every [`SemanticCall`](call::Operation) charges its declared
//!   cost against the caller's budget before executing; insufficient → `BudgetExceeded`.
//! - **INV-2 (capability first)** — every side-effecting call carries a valid [`CapabilityToken`]
//!   granting the required [`Permission`] *and* a [`Scope`] covering the target; else `CapabilityDenied`.
//!   Checking permission class **without** checking scope is non-conformant.
//! - **INV-3 (vector fidelity)** — a [`VectorMessage`] passes losslessly only between equal
//!   [`ModelFingerprint`]s; otherwise translation is explicit and its loss is measurable.
//!   Implicit lossy conversion is forbidden.
//! - **INV-4 (auditable)** — every call emits an immutable [`AuditRecord`]
//!   (who · which capability · how much budget · acting on what).
//! - **INV-5 (human as the floor)** — a single [`Permission::Sovereign`] capability, held only by
//!   the human supervisory plane, can unconditionally suspend / snapshot / roll back / terminate
//!   any agent. No implementation may remove it.
//!
//! [`Permission`]: capability::Permission
//! [`Scope`]: capability::Scope
//! [`CapabilityToken`]: capability::CapabilityToken
//! [`ModelFingerprint`]: message::ModelFingerprint

pub mod agent;
pub mod budget;
pub mod call;
pub mod capability;
pub mod error;
pub mod message;
pub mod space;
pub mod tool;

pub use agent::AgentId;
pub use budget::{AttentionBudget, RefillPolicy};
pub use call::{AuditRecord, Operation};
pub use capability::{CapabilityToken, CapabilityVerifier, Permission, ResourceKind, Scope};
pub use error::TamError;
pub use message::{
    Dtype, IntentGroup, IntentVector, MessageKind, ModelFingerprint, Recipient, VectorMessage,
    VectorPayload,
};
pub use space::{SemanticObject, SemanticSpace};
pub use tool::{Tool, ToolResult};
