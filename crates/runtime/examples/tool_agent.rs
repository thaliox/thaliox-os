//! # tool_agent — agent 调 web_search / fetch
//!
//! agent 通过工具触达外界(搜索 / 抓取),每次 ToolInvoke 都过 Execute 能力
//! 门控 + 预算对账 + 审计;搜索结果写进记忆并可检索。
//!
//! ```bash
//! TAVILY_API_KEY=tvly-...  cargo run -p thaliox-runtime --example tool_agent
//! #   无 key 时跳过 web_search;fetch 始终可用(任意 URL)
//! ```

use std::sync::Arc;

use thaliox_cognition::MockProvider;
use thaliox_core::{
    AgentId, AttentionBudget, CapabilityToken, Permission, ResourceKind, Scope, SemanticObject,
};
use thaliox_memory::InMemorySpace;
use thaliox_runtime::{Action, Agent, Outcome};
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

#[tokio::main]
async fn main() {
    let memory = Arc::new(InMemorySpace::new());
    let mut agent = Agent::new(
        AgentId::new("agent-tool"),
        AttentionBudget::new(20_000, 1_000_000),
        memory.clone(),
        Arc::new(MockProvider::new("(local)", 1)),
    )
    .with_tool(Arc::new(Fetch::new()))
    .grant(cap(
        "agent-tool",
        Permission::Execute,
        ResourceKind::Tool,
        "tool://*",
    ))
    .grant(cap(
        "agent-tool",
        Permission::Write,
        ResourceKind::Memory,
        "mem://agent-tool/*",
    ))
    .grant(cap(
        "agent-tool",
        Permission::Read,
        ResourceKind::Memory,
        "mem://agent-tool/*",
    ));

    let has_search = match WebSearch::from_env() {
        Ok(ws) => {
            agent = agent.with_tool(Arc::new(ws));
            true
        }
        Err(_) => false,
    };
    agent.start().unwrap();

    println!("→ THALIOX 工具系统 — agent 调 web_search / fetch");
    println!(
        "agent: {}  能力: Execute(tool://*) · Write/Read(mem)\n",
        agent.id()
    );

    // web_search(需 TAVILY_API_KEY)
    if has_search {
        let q = "THALIOX AI-native operating system";
        match agent
            .act(Action::Invoke {
                tool: "web_search".into(),
                input: q.into(),
                cost: 200,
            })
            .await
        {
            Ok(Outcome::Invoked(out)) => {
                println!("· web_search \"{q}\" →");
                for line in out.lines().take(8) {
                    println!("    {line}");
                }
                println!("  (余 {})", agent.remaining_budget());
                // 把搜索结果写进记忆
                let obj = SemanticObject {
                    id: "search/thaliox".into(),
                    vector: vec![0.1, 0.2, 0.3],
                    tags: vec!["search".into()],
                    data: out.into_bytes(),
                    capability: None,
                };
                let _ = agent
                    .act(Action::Remember {
                        object: obj,
                        cost: 5,
                    })
                    .await;
                println!("· remember 搜索结果 → 余 {}", agent.remaining_budget());
            }
            Err(e) => println!("· web_search ✗ {e}"),
            _ => {}
        }
    } else {
        println!("(无 TAVILY_API_KEY,跳过 web_search;设 TAVILY_API_KEY 启用)");
    }

    // fetch(真实,无需 key)
    let url = "https://example.com";
    match agent
        .act(Action::Invoke {
            tool: "fetch".into(),
            input: url.into(),
            cost: 100,
        })
        .await
    {
        Ok(Outcome::Invoked(out)) => {
            let title = out
                .lines()
                .find(|l| l.contains("<title>"))
                .unwrap_or("")
                .trim();
            println!("\n· fetch {url} → {} 字符  {title}", out.chars().count());
            println!("  (余 {})", agent.remaining_budget());
        }
        Err(e) => println!("\n· fetch ✗ {e}"),
        _ => {}
    }

    // 检索回刚才写入的搜索结果
    if has_search
        && let Ok(Outcome::Recalled(hits)) = agent
            .act(Action::Recall {
                query: vec![0.1, 0.2, 0.3],
                k: 1,
                cost: 4,
            })
            .await
        && let Some(o) = hits.first()
    {
        println!(
            "\n· recall → 召回 '{}' ({} 字节搜索结果)",
            o.id,
            o.data.len()
        );
    }

    println!("\n审计 {} 条(全程 INV-1/2/4 门控):", agent.audit().len());
    for r in agent.audit() {
        let mark = if r.allowed { "✓" } else { "✗" };
        println!("  {mark} {:?} cost={} {}", r.op, r.cost, r.target);
    }
}
