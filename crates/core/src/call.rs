//! The **SemanticCall** — the TAM "instruction". Every operation is gated
//! (INV-1 charge budget · INV-2 capability check where required · act on state)
//! and emits an audit record (INV-4). (TAM §2, §7)

use serde::{Deserialize, Serialize};

use crate::agent::AgentId;
use crate::capability::Permission;

/// The operation set. `Think` (internal cognition) is budget-only; the rest are
/// side-effecting and capability-gated (TAM §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    /// Internal cognition (inference). Costs budget, needs no capability.
    Think,
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
    /// Lifecycle governance: suspend / roll back / terminate any agent. Held by
    /// the control plane (M5), not reserved to any party outside the system.
    Govern,
}

impl Operation {
    /// The permission this operation requires (TAM §7), or `None` if it is not
    /// capability-gated (only `Think`, an agent's own introspection).
    pub fn required_permission(self) -> Option<Permission> {
        use Operation::*;
        Some(match self {
            Think => return None,
            VSend | VRecv => Permission::Communicate,
            MemRead | MemSearch => Permission::Read,
            MemWrite | MemSummarize => Permission::Write,
            ToolInvoke => Permission::Execute,
            AgentSpawn => Permission::Spawn,
            // delegate/revoke are further constrained by holding a *delegable*
            // token; Admin is the class gate.
            CapDelegate | CapRevoke | Checkpoint | Restore | Govern => Permission::Admin,
        })
    }
}

/// **INV-4**: the immutable record every SemanticCall emits, retrievable by the
/// control plane (and any supervisor it authorizes) for governance and
/// self-optimization — it is also the learned policy's training ledger (RFC-0007).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Who made the call.
    pub agent: AgentId,
    /// What operation.
    pub op: Operation,
    /// Which permission authorized it (`None` for budget-only `Think`).
    pub permission_used: Option<Permission>,
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
        assert_eq!(Operation::Think.required_permission(), None);
        assert_eq!(
            Operation::VSend.required_permission(),
            Some(Permission::Communicate)
        );
        assert_eq!(
            Operation::MemSearch.required_permission(),
            Some(Permission::Read)
        );
        assert_eq!(
            Operation::MemWrite.required_permission(),
            Some(Permission::Write)
        );
        assert_eq!(
            Operation::Govern.required_permission(),
            Some(Permission::Admin)
        );
    }
}
