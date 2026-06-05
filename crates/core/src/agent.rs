//! Agent identity — the TAM execution unit's globally-unique semantic address.

use serde::{Deserialize, Serialize};

/// A globally-unique semantic address for an agent,
/// e.g. `thaliox://team-alpha/researcher-07`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    /// Wrap a semantic address.
    pub fn new(addr: impl Into<String>) -> Self {
        Self(addr.into())
    }

    /// The address as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
