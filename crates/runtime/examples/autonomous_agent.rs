//! # autonomous_agent — LLM tool-calling 闭环
//!
//! 给 agent 一个目标,**模型自己决定调哪个工具**(web_search / fetch),执行、把
//! 结果喂回、再思考,直到给出文本答案。每轮思考受预算门控,每次工具调用走完整
//! 能力门控 + 审计。无 LLM key 时用脚本化 mock 演示同一条闭环。
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
        // 离线:脚本化 mock,演示同一条闭环(调一次工具再答)。
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
            Completion::text("(离线 mock)已调用工具并完成。", 12),
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
        "用 web_search 查 'THALIOX AI-native operating system' 是什么,再用一句中文总结你看到的内容。"
    } else {
        "用 fetch 抓取 https://example.com 的内容,再用一句话说明它是什么。"
    };

    println!("→ THALIOX 自主 agent(LLM tool-calling 闭环)");
    println!(
        "  model '{model}'  {}  工具: fetch{}\n",
        if live {
            "(真实 LLM)"
        } else {
            "(离线 mock)"
        },
        if has_tavily { " · web_search" } else { "" }
    );
    println!("目标: {goal}\n");

    match agent.run(goal, 6).await {
        Ok(answer) => println!("最终答案: {}", answer.trim()),
        Err(e) => println!("失败: {e}"),
    }

    println!("\n模型自主决策轨迹(审计 {} 条):", agent.audit().len());
    for r in agent.audit() {
        let mark = if r.allowed { "✓" } else { "✗" };
        println!("  {mark} {:?} cost={} {}", r.op, r.cost, r.target);
    }
    println!("余 {}", agent.remaining_budget());
}
