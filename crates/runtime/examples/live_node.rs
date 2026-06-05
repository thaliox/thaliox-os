//! # live_node — a real LLM driving an agent
//!
//! The same pipeline as `single_node`, but cognition connects to a **real model**
//! (Anthropic / OpenAI, or any compatible gateway). Without an API key it
//! gracefully degrades to an offline mock, so it runs anytime.
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-...  cargo run -p thaliox-runtime --example live_node
//! #   Optional: OPENAI_API_KEY / ANTHROPIC_BASE_URL / OPENAI_BASE_URL / THALIOX_MODEL
//! ```

use std::sync::Arc;

use thaliox_cognition::{AnthropicProvider, LlmProvider, MockProvider, OpenAiProvider};
use thaliox_core::{
    AgentId, AttentionBudget, CapabilityToken, Permission, ResourceKind, Scope, SemanticObject,
};
use thaliox_memory::InMemorySpace;
use thaliox_runtime::{Action, Agent, Outcome};

fn cap(subject: &str, perm: Permission, pattern: &str) -> CapabilityToken {
    CapabilityToken {
        subject: AgentId::new(subject),
        permissions: vec![perm],
        scope: vec![Scope {
            resource: ResourceKind::Memory,
            pattern: pattern.into(),
        }],
        issued_at: 0,
        expires_at: 0,
        jti: [1; 16],
        delegable: false,
        signature: [0; 32],
    }
}

/// Pick a provider from whichever API key is present; else an offline mock.
fn pick_provider() -> (Arc<dyn LlmProvider>, String, bool) {
    let model_env = std::env::var("THALIOX_MODEL").ok();
    let max_tokens: u32 = std::env::var("THALIOX_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024);
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        let model = model_env.unwrap_or_else(|| "claude-sonnet-4-6".into());
        let mut p = AnthropicProvider::from_env(model.as_str())
            .unwrap()
            .with_max_tokens(max_tokens);
        if let Ok(url) = std::env::var("ANTHROPIC_BASE_URL") {
            p = p.with_base_url(url);
        }
        (Arc::new(p), model, true)
    } else if std::env::var("OPENAI_API_KEY").is_ok() {
        let model = model_env.unwrap_or_else(|| "gpt-4o".into());
        let mut p = OpenAiProvider::from_env(model.as_str())
            .unwrap()
            .with_max_tokens(max_tokens);
        if let Ok(url) = std::env::var("OPENAI_BASE_URL") {
            p = p.with_base_url(url);
        }
        (Arc::new(p), model, true)
    } else {
        let mock = MockProvider::new(
            "THALIOX: an AI-native, AI-self-managed operating system.",
            12,
        );
        (Arc::new(mock), "local-mock".into(), false)
    }
}

#[tokio::main]
async fn main() {
    let (provider, model, live) = pick_provider();
    println!("→ THALIOX cognition live");
    println!(
        "  provider '{}'  model '{}'  {}",
        provider.id(),
        model,
        if live {
            "(real LLM)"
        } else {
            "(no key → degraded to offline mock)"
        }
    );
    if !live {
        println!(
            "  set ANTHROPIC_API_KEY or OPENAI_API_KEY to connect a real model (optional *_BASE_URL / THALIOX_MODEL)"
        );
    }
    println!();

    let memory = Arc::new(InMemorySpace::new());
    let mut agent = Agent::new(
        AgentId::new("agent-live"),
        AttentionBudget::new(5000, 100_000),
        memory.clone(),
        provider,
    )
    .grant(cap("agent-live", Permission::Write, "mem://agent-live/*"))
    .grant(cap("agent-live", Permission::Read, "mem://agent-live/*"));
    agent.start().unwrap();

    // think —— real inference
    let prompt = "Define an AI-native operating system (THALIOX) in one sentence.";
    let thought = match agent
        .act(Action::Think {
            prompt: prompt.into(),
            cost: 500,
        })
        .await
    {
        Ok(Outcome::Thought(c)) => {
            println!(
                "· think  → \"{}\"\n          (real tokens={}, declared cost=500, remaining {})",
                c.content.trim(),
                c.tokens,
                agent.remaining_budget()
            );
            c.content
        }
        Err(e) => {
            eprintln!("· think  ✗ {e}");
            return;
        }
        _ => return,
    };

    // remember —— write the thought into memory (gated by Write)
    let obj = SemanticObject {
        id: "thought-1".into(),
        vector: vec![0.1, 0.2, 0.3],
        tags: vec!["thought".into()],
        data: thought.into_bytes(),
        capability: None,
    };
    if let Ok(Outcome::Remembered(id)) = agent
        .act(Action::Remember {
            object: obj,
            cost: 5,
        })
        .await
    {
        println!(
            "· remember → remembered '{id}'  remaining {}",
            agent.remaining_budget()
        );
    }

    // recall —— retrieve it back (gated by Read)
    if let Ok(Outcome::Recalled(hits)) = agent
        .act(Action::Recall {
            query: vec![0.1, 0.2, 0.3],
            k: 1,
            cost: 4,
        })
        .await
        && let Some(o) = hits.first()
    {
        println!(
            "· recall → recalled '{}': \"{}\"",
            o.id,
            String::from_utf8_lossy(&o.data).trim()
        );
    }

    println!(
        "\n{} audit records (real LLM → agent → memory, fully gated by INV-1/2/4):",
        agent.audit().len()
    );
    for r in agent.audit() {
        let mark = if r.allowed { "✓" } else { "✗" };
        println!("  {mark} {:?} cost={} {}", r.op, r.cost, r.target);
    }
}
