//! # THALIOX cognition (L1)
//!
//! The unified cognition interface (MASTER_PLAN §1.3, F5): one [`LlmProvider`]
//! over remote backends *and* a local fallback, with **tool calling** — the
//! model may decide to call a [`ToolSpec`] and the runtime executes it, closing
//! the cognition → tools → memory loop. Token usage feeds the attention budget.

pub mod anthropic;
/// RFC-0003 §5 falsification gates for the MELD pillars (E1 mergeable cognition,
/// E2 energy-based readout).
pub mod experiment;
pub mod openai;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use thaliox_core::TamError;

/// A tool call the model requested (name + JSON arguments + a correlation id).
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Arguments as a JSON object string.
    pub arguments: String,
}

/// A tool advertised to the model: name, description, and a JSON-Schema for args.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// A turn in a cognition exchange.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool calls the assistant issued this turn (assistant turns only).
    pub tool_calls: Vec<ToolCall>,
    /// For a tool-result message, the id of the call it answers.
    pub tool_call_id: Option<String>,
}

impl Message {
    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn system(c: impl Into<String>) -> Self {
        Self::plain(Role::System, c)
    }
    pub fn user(c: impl Into<String>) -> Self {
        Self::plain(Role::User, c)
    }
    /// An assistant turn that issued tool calls.
    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
        }
    }
    /// A tool-result message answering the call `call_id`.
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
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

/// The result of a completion: text and/or tool calls, plus token cost.
#[derive(Debug, Clone)]
pub struct Completion {
    pub content: String,
    /// Tokens consumed — charged against the agent's attention budget (INV-1).
    pub tokens: u64,
    /// Tool calls the model wants executed (empty when it answered in text).
    pub tool_calls: Vec<ToolCall>,
}

impl Completion {
    /// A plain text completion.
    pub fn text(content: impl Into<String>, tokens: u64) -> Self {
        Self {
            content: content.into(),
            tokens,
            tool_calls: Vec::new(),
        }
    }
    /// A completion that requests tool calls.
    pub fn calls(tokens: u64, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            content: String::new(),
            tokens,
            tool_calls,
        }
    }
}

/// A cognition backend: remote API or a local quantized model.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Provider id (e.g. `"anthropic"`, `"local-mock"`).
    fn id(&self) -> &str;

    /// Whether this provider works without network (a local model).
    fn is_local(&self) -> bool;

    /// Complete a conversation, advertising `tools` the model may call.
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<Completion, TamError>;
}

/// A deterministic, local, offline cognition stand-in (F5). Either a fixed reply
/// or a FIFO script of completions (for testing tool-calling loops).
pub struct MockProvider {
    mode: MockMode,
}

enum MockMode {
    Fixed { reply: String, tokens: u64 },
    Scripted(Mutex<VecDeque<Completion>>),
}

impl MockProvider {
    /// Always reply `reply`, costing `tokens_per_call` tokens.
    pub fn new(reply: impl Into<String>, tokens_per_call: u64) -> Self {
        Self {
            mode: MockMode::Fixed {
                reply: reply.into(),
                tokens: tokens_per_call,
            },
        }
    }

    /// Return `completions` in order (FIFO) — useful to script a tool-call then
    /// a final text answer.
    pub fn scripted(completions: Vec<Completion>) -> Self {
        Self {
            mode: MockMode::Scripted(Mutex::new(completions.into())),
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

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSpec],
    ) -> Result<Completion, TamError> {
        match &self.mode {
            MockMode::Fixed { reply, tokens } => Ok(Completion::text(reply.clone(), *tokens)),
            MockMode::Scripted(q) => Ok(q
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Completion::text("(no more scripted responses)", 1))),
        }
    }
}
