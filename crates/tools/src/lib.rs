//! # THALIOX tools — an agent's external actions
//!
//! Concrete [`Tool`](thaliox_core::Tool)s an agent can invoke via a `ToolInvoke`
//! SemanticCall (gated by the `Execute` permission, TAM §7):
//!
//! - [`Fetch`] — HTTP GET a URL, return the body (truncated).
//! - [`WebSearch`] — web search via the Tavily API (agent-oriented search).
//!
//! Each reports a token-equivalent cost so the runtime can reconcile the
//! attention budget (INV-1).

pub mod fetch;
pub mod search;

pub use fetch::Fetch;
pub use search::WebSearch;
