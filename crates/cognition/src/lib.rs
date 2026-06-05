//! # THALIOX cognition (L1)
//!
//! The unified cognition interface (MASTER_PLAN §1.3, F5): one
//! [`LlmProvider`] abstraction over remote backends *and* local quantized
//! models, so an agent keeps reasoning offline (built-in small model). Token
//! usage feeds the [`AttentionBudget`](thaliox_core::AttentionBudget) (TAM §4).
//!
//! M1 status: skeleton — the provider trait only.

use async_trait::async_trait;
use thaliox_core::TamError;

/// A turn in a cognition exchange.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// Conversation role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// The result of a completion, including the tokens it cost (for budget charging).
#[derive(Debug, Clone)]
pub struct Completion {
    pub content: String,
    /// Tokens consumed — charged against the agent's attention budget (INV-1).
    pub tokens: u64,
}

/// A cognition backend: remote API or a local quantized model. Offline mode
/// falls back to a built-in model so the agent never goes dark.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Provider id (e.g. `"anthropic"`, `"local-gguf"`).
    fn id(&self) -> &str;

    /// Whether this provider works without network (a local model).
    fn is_local(&self) -> bool;

    /// Complete a conversation.
    async fn complete(&self, messages: &[Message]) -> Result<Completion, TamError>;
}
