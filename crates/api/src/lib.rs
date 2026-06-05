//! # THALIOX api (L5)
//!
//! The unified API gateway (axum/tonic) plus the multi-language SDK surface
//! (MASTER_PLAN §3 item 11, F6). It is the human / client entry point and maps
//! external requests onto TAM SemanticCalls and runtime lifecycle ops — every
//! one budget-charged, capability-checked, and audited (INV-1/2/4).
//!
//! M1 status: skeleton — the operation surface only; the axum/tonic server is
//! wired when M1's single-node MVP lands.

use thaliox_core::AgentId;

/// Top-level gateway operations exposed to clients and SDKs.
#[derive(Debug, Clone)]
pub enum GatewayOp {
    /// One-click deploy an agent image (F2).
    Deploy { image: String },
    /// Spawn an agent instance.
    Spawn { id: AgentId },
    /// Ask a running agent something.
    Ask { id: AgentId, prompt: String },
    /// Snapshot an agent's recoverable state.
    Checkpoint { id: AgentId },
    /// Human-only supervisory control (suspend / roll back / terminate) — INV-5.
    Sovereign { target: AgentId },
}
