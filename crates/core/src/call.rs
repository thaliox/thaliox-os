//! The **SemanticCall** — the TAM "instruction". Every operation is triple-gated
//! (INV-1 charge budget · INV-2 capability check · act on state) and emits an
//! audit record (INV-4). (TAM §2, §7)

use serde::{Deserialize, Serialize};

use crate::agent::AgentId;
use crate::capability::Permission;

/// The minimal operation set (TAM §7). Each maps to a required [`Permission`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    /// Send a vector message.
    VSend,
    /// Receive a vector message.
    VRecv,
    /// Read a memory object.
    MemRead,
    /// Semantic search over memory.
    MemSearch,
    /// Write a memory object.
    MemWrite,
    /// Summarize memory into a concept.
    MemSummarize,
    /// Invoke a tool (web_search / fetch / …).
    ToolInvoke,
    /// Spawn a sub-agent.
    AgentSpawn,
    /// Delegate a capability to a sub-agent.
    CapDelegate,
    /// Revoke a delegated capability.
    CapRevoke,
    /// Snapshot an agent's complete recoverable state.
    Checkpoint,
    /// Restore an agent from a checkpoint.
    Restore,
    /// Human-only: suspend / roll back / terminate any agent.
    Sovereign,
}

impl Operation {
    /// The permission this operation requires (TAM §7).
    pub fn required_permission(self) -> Permission {
        use Operation::*;
        match self {
            VSend | VRecv => Permission::Communicate,
            MemRead | MemSearch => Permission::Read,
            MemWrite | MemSummarize => Permission::Write,
            ToolInvoke => Permission::Execute,
            AgentSpawn => Permission::Spawn,
            // delegate/revoke are further constrained by holding a *delegable*
            // token; Admin is the class gate.
            CapDelegate | CapRevoke | Checkpoint | Restore => Permission::Admin,
            Sovereign => Permission::Sovereign,
        }
    }
}

/// **INV-4**: the immutable record every SemanticCall emits, retrievable by the
/// human supervisory plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Who made the call.
    pub agent: AgentId,
    /// What operation.
    pub op: Operation,
    /// Which permission authorized it.
    pub permission_used: Permission,
    /// How much attention budget it cost (tokens).
    pub cost: u64,
    /// What it acted on (resource target).
    pub target: String,
    /// When (unix millis).
    pub at: u64,
    /// Whether it was allowed.
    pub allowed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_map_to_permissions() {
        assert_eq!(
            Operation::VSend.required_permission(),
            Permission::Communicate
        );
        assert_eq!(Operation::MemSearch.required_permission(), Permission::Read);
        assert_eq!(
            Operation::AgentSpawn.required_permission(),
            Permission::Spawn
        );
        assert_eq!(
            Operation::Sovereign.required_permission(),
            Permission::Sovereign
        );
    }
}
