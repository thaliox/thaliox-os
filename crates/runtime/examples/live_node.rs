//! # live_node — 真实 LLM 驱动一个 agent
//!
//! 与 `single_node` 同一条链路,但 cognition 接**真实模型**(Anthropic / OpenAI,
//! 或任意兼容网关)。无 API key 时优雅退化到离线 mock,所以随时可跑。
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-...  cargo run -p thaliox-runtime --example live_node
//! #   可选: OPENAI_API_KEY / ANTHROPIC_BASE_URL / OPENAI_BASE_URL / THALIOX_MODEL
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
        let mock = MockProvider::new("THALIOX:一个为 AI 原生设计、AI 自管理的操作系统。", 12);
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
            "(真实 LLM)"
        } else {
            "(无 key → 离线 mock 退化)"
        }
    );
    if !live {
        println!(
            "  设 ANTHROPIC_API_KEY 或 OPENAI_API_KEY 接真实模型(可选 *_BASE_URL / THALIOX_MODEL)"
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

    // think —— 真实推理
    let prompt = "用一句话定义 AI 原生操作系统(THALIOX)。";
    let thought = match agent
        .act(Action::Think {
            prompt: prompt.into(),
            cost: 500,
        })
        .await
    {
        Ok(Outcome::Thought(c)) => {
            println!(
                "· think  → \"{}\"\n          (真实 token={}, 声明成本=500, 余 {})",
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

    // remember —— 把思考写进记忆(受 Write 门控)
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
            "· remember → 已记忆 '{id}'  余 {}",
            agent.remaining_budget()
        );
    }

    // recall —— 检索回来(受 Read 门控)
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
            "· recall → 召回 '{}': \"{}\"",
            o.id,
            String::from_utf8_lossy(&o.data).trim()
        );
    }

    println!(
        "\n审计 {} 条(真实 LLM → agent → memory,全程 INV-1/2/4 门控):",
        agent.audit().len()
    );
    for r in agent.audit() {
        let mark = if r.allowed { "✓" } else { "✗" };
        println!("  {mark} {:?} cost={} {}", r.op, r.cost, r.target);
    }
}
