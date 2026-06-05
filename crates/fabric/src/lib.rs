//! # THALIOX fabric (L3)
//!
//! agent↔agent communication, team orchestration, and CRDT state replication
//! (MASTER_PLAN §2). Transport carries [`VectorMessage`](thaliox_core::VectorMessage)s
//! (TAM §3): near-term over gRPC/QUIC, long-term the `vsend`/`vrecv` hardware
//! primitive. A **team** is a *holarchy* — agents that are whole yet compose
//! into a larger whole.
//!
//! M1 status: skeleton — transport + team contracts only.

use async_trait::async_trait;
use thaliox_core::{AgentId, TamError, VectorMessage};

/// Carries vector messages between agents.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a vector message (unicast or multicast to an intent group).
    async fn send(&self, msg: VectorMessage) -> Result<(), TamError>;

    /// Receive the next inbound vector message.
    async fn recv(&self) -> Result<VectorMessage, TamError>;
}

/// Composable collaboration paradigms a team may adopt (MASTER_PLAN §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paradigm {
    Hierarchy,
    Market,
    Swarm,
    Pipeline,
}

/// A team: a holarchy of agents with shared goals and assigned roles.
#[derive(Debug, Clone)]
pub struct Team {
    pub name: String,
    pub members: Vec<AgentId>,
    pub paradigm: Paradigm,
}
