//! # autonomous_agent — LLM tool-calling loop
//!
//! Give the agent a goal and **the model itself decides which tool to call**
//! (web_search / fetch), executes it, feeds the result back, thinks again, until
//! it produces a text answer. Each think step is budget-gated; each tool call
//! goes through the full capability gate + audit. Without an LLM key, a scripted
//! mock demonstrates the same loop.
//!
//! ```bash
//! OPENAI_API_KEY=...  OPENAI_BASE_URL=...  THALIOX_MODEL=glm-5.1 \
//!   TAVILY_API_KEY=tvly-...  cargo run -p thaliox-runtime --example autonomous_agent
//! ```

use std::sync::Arc;

use thaliox_cognition::{
    AnthropicProvider, Completion, LlmProvider, MockProvider, OpenAiProvider, ToolCall,
};
use thaliox_core::{AgentId, AttentionBudget, CapabilityToken, Permission, ResourceKind, Scope};
use thaliox_memory::InMemorySpace;
use thaliox_runtime::Agent;
use thaliox_tools::{Fetch, WebSearch};

fn cap(subject: &str, perm: Permission, resource: ResourceKind, pattern: &str) -> CapabilityToken {
    CapabilityToken {
        subject: AgentId::new(subject),
        permissions: vec![perm],
        scope: vec![Scope {
            resource,
            pattern: pattern.into(),
        }],
        issued_at: 0,
        expires_at: 0,
        jti: [1; 16],
        delegable: false,
        signature: [0; 32],
    }
}

fn pick_provider(has_tavily: bool) -> (Arc<dyn LlmProvider>, String, bool) {
    let model_env = std::env::var("THALIOX_MODEL").ok();
    let max = std::env::var("THALIOX_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2048);
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        let m = model_env.unwrap_or_else(|| "claude-sonnet-4-6".into());
        let mut p = AnthropicProvider::from_env(m.as_str())
            .unwrap()
            .with_max_tokens(max);
        if let Ok(u) = std::env::var("ANTHROPIC_BASE_URL") {
            p = p.with_base_url(u);
        }
        (Arc::new(p), m, true)
    } else if std::env::var("OPENAI_API_KEY").is_ok() {
        let m = model_env.unwrap_or_else(|| "gpt-4o".into());
        let mut p = OpenAiProvider::from_env(m.as_str())
            .unwrap()
            .with_max_tokens(max);
        if let Ok(u) = std::env::var("OPENAI_BASE_URL") {
            p = p.with_base_url(u);
        }
        (Arc::new(p), m, true)
    } else {
        // Offline: a scripted mock demonstrating the same loop (call a tool once, then answer).
        let (name, input) = if has_tavily {
            ("web_search", "THALIOX AI operating system")
        } else {
            ("fetch", "https://example.com")
        };
        let mock = MockProvider::scripted(vec![
            Completion::calls(
                30,
                vec![ToolCall {
                    id: "c1".into(),
                    name: name.into(),
                    arguments: format!(r#"{{"input":"{input}"}}"#),
                }],
            ),
            Completion::text("(offline mock) called the tool and finished.", 12),
        ]);
        (Arc::new(mock), "local-mock(scripted)".into(), false)
    }
}

#[tokio::main]
async fn main() {
    let has_tavily = std::env::var("TAVILY_API_KEY").is_ok();
    let (provider, model, live) = pick_provider(has_tavily);

    let mut agent = Agent::new(
        AgentId::new("agent-auto"),
        AttentionBudget::new(50_000, 1_000_000),
        Arc::new(InMemorySpace::new()),
        provider,
    )
    .with_tool(Arc::new(Fetch::new()))
    .grant(cap(
        "agent-auto",
        Permission::Execute,
        ResourceKind::Tool,
        "tool://*",
    ))
    .grant(cap(
        "agent-auto",
        Permission::Write,
        ResourceKind::Memory,
        "mem://agent-auto/*",
    ))
    .grant(cap(
        "agent-auto",
        Permission::Read,
        ResourceKind::Memory,
        "mem://agent-auto/*",
    ));
    if has_tavily && let Ok(ws) = WebSearch::from_env() {
        agent = agent.with_tool(Arc::new(ws));
    }
    agent.start().unwrap();

    let goal = if has_tavily {
        "Use web_search to find out what 'THALIOX AI-native operating system' is, then summarize what you saw in one sentence."
    } else {
        "Use fetch to retrieve the content of https://example.com, then explain in one sentence what it is."
    };

    println!("→ THALIOX autonomous agent (LLM tool-calling loop)");
    println!(
        "  model '{model}'  {}  tools: fetch{}\n",
        if live { "(real LLM)" } else { "(offline mock)" },
        if has_tavily { " · web_search" } else { "" }
    );
    println!("goal: {goal}\n");

    match agent.run(goal, 6).await {
        Ok(answer) => println!("final answer: {}", answer.trim()),
        Err(e) => println!("failed: {e}"),
    }

    println!(
        "\nmodel's autonomous decision trail ({} audit records):",
        agent.audit().len()
    );
    for r in agent.audit() {
        let mark = if r.allowed { "✓" } else { "✗" };
        println!("  {mark} {:?} cost={} {}", r.op, r.cost, r.target);
    }
    println!("remaining {}", agent.remaining_budget());
}
