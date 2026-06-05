//! **Tool** — an agent's action on the outside world (web_search / fetch / …).
//! A tool invocation is a `ToolInvoke` SemanticCall, gated by the `Execute`
//! permission (MASTER_PLAN §1.3, TAM §7). The trait lives in `core` (the
//! contract); concrete tools live in `thaliox-tools`.

use async_trait::async_trait;

use crate::error::TamError;

/// The result of invoking a tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Textual output the agent can reason over / remember.
    pub output: String,
    /// Token-equivalent cost, used to reconcile the attention budget (INV-1).
    /// (TAM §9 open question: how to convert non-inference ops to token-equivalents.)
    pub cost: u64,
}

/// An external action available to an agent. One call = one `ToolInvoke`.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The tool name — used for scope matching, e.g. `"web_search"`.
    fn name(&self) -> &str;

    /// A natural-language description shown to the model when this tool is
    /// advertised for tool-calling. Empty by default.
    fn description(&self) -> &str {
        ""
    }

    /// Execute the tool with `input` (a query, a URL, …).
    async fn invoke(&self, input: &str) -> Result<ToolResult, TamError>;
}
