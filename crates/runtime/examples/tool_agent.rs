//! # tool_agent — agent calls web_search / fetch
//!
//! The agent reaches the outside world through tools (search / fetch); every
//! ToolInvoke passes the Execute capability gate + budget reconciliation +
//! audit; search results are written into memory and can be retrieved.
//!
//! ```bash
//! TAVILY_API_KEY=tvly-...  cargo run -p thaliox-runtime --example tool_agent
//! #   Without a key, web_search is skipped; fetch is always available (any URL)
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

    println!("→ THALIOX tool system — agent calls web_search / fetch");
    println!(
        "agent: {}  capabilities: Execute(tool://*) · Write/Read(mem)\n",
        agent.id()
    );

    // web_search (requires TAVILY_API_KEY)
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
                println!("  (remaining {})", agent.remaining_budget());
                // Write the search results into memory
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
                println!(
                    "· remember search results → remaining {}",
                    agent.remaining_budget()
                );
            }
            Err(e) => println!("· web_search ✗ {e}"),
            _ => {}
        }
    } else {
        println!("(no TAVILY_API_KEY, skipping web_search; set TAVILY_API_KEY to enable)");
    }

    // fetch (real, no key needed)
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
            println!("\n· fetch {url} → {} chars  {title}", out.chars().count());
            println!("  (remaining {})", agent.remaining_budget());
        }
        Err(e) => println!("\n· fetch ✗ {e}"),
        _ => {}
    }

    // Retrieve the search results just written
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
            "\n· recall → recalled '{}' ({} bytes of search results)",
            o.id,
            o.data.len()
        );
    }

    println!(
        "\n{} audit records (fully gated by INV-1/2/4):",
        agent.audit().len()
    );
    for r in agent.audit() {
        let mark = if r.allowed { "✓" } else { "✗" };
        println!("  {mark} {:?} cost={} {}", r.op, r.cost, r.target);
    }
}
