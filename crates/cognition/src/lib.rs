//! # THALIOX cognition (L1)
//!
//! The unified cognition interface (MASTER_PLAN §1.3, F5): one
//! [`LlmProvider`] abstraction over remote backends *and* local quantized
//! models, so an agent keeps reasoning offline (built-in small model). Token
//! usage feeds the [`AttentionBudget`](thaliox_core::AttentionBudget) (TAM §4).
//!
//! M1 status: the provider trait + a local [`MockProvider`] (the offline
//! fallback stand-in); real remote / GGUF providers slot in behind the trait.

pub mod anthropic;
pub mod openai;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;

use async_trait::async_trait;
use thaliox_core::TamError;

/// A turn in a cognition exchange.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(c: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: c.into(),
        }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: c.into(),
        }
    }
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

/// A deterministic, local, offline cognition stand-in for M1: echoes a fixed
/// reply and reports a fixed token cost. Stands in for the built-in small model
/// until a real GGUF backend lands (F5).
pub struct MockProvider {
    reply: String,
    tokens_per_call: u64,
}

impl MockProvider {
    /// A provider that always replies `reply`, costing `tokens_per_call` tokens.
    pub fn new(reply: impl Into<String>, tokens_per_call: u64) -> Self {
        Self {
            reply: reply.into(),
            tokens_per_call,
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn id(&self) -> &str {
        "local-mock"
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn complete(&self, _messages: &[Message]) -> Result<Completion, TamError> {
        Ok(Completion {
            content: self.reply.clone(),
            tokens: self.tokens_per_call,
        })
    }
}
