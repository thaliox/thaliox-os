//! # single-node MVP — runtime driving one agent through the full pipeline
//!
//! cognition → memory → attention budget → capability check; every operation
//! passes the TAM triple gate (INV-2 capability · INV-1 budget · INV-4 audit).
//! Runs offline (local MockProvider).
//!
//! ```bash
//! cargo run -p thaliox-runtime --example single_node
//! ```

use std::sync::Arc;

use thaliox_cognition::MockProvider;
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

fn note(id: &str, vector: Vec<f32>, text: &str) -> SemanticObject {
    SemanticObject {
        id: id.into(),
        vector,
        tags: vec!["note".into()],
        data: text.as_bytes().to_vec(),
        capability: None,
    }
}

/// Execute an action and print the result (success charges budget, failure is denied by the gate).
async fn step(agent: &mut Agent, label: &str, action: Action) {
    match agent.act(action).await {
        Ok(Outcome::Thought(c)) => println!(
            "· {label:<28} → think: \"{}\"  (-{} tok)  remaining {}",
            c.content,
            c.tokens,
            agent.remaining_budget()
        ),
        Ok(Outcome::Remembered(id)) => println!(
            "· {label:<28} → remembered '{id}'  remaining {}",
            agent.remaining_budget()
        ),
        Ok(Outcome::Recalled(hits)) => {
            let ids: Vec<&str> = hits.iter().map(|o| o.id.as_str()).collect();
            println!(
                "· {label:<28} → recalled {} {ids:?}  remaining {}",
                hits.len(),
                agent.remaining_budget()
            );
        }
        Ok(Outcome::Invoked(out)) => println!(
            "· {label:<28} → tool output: {}  remaining {}",
            out.chars().take(50).collect::<String>(),
            agent.remaining_budget()
        ),
        Err(e) => println!(
            "· {label:<28} ✗ denied: {e}  remaining {}",
            agent.remaining_budget()
        ),
    }
}

#[tokio::main]
async fn main() {
    // memory (L1) + cognition (L1, local offline)
    let memory = Arc::new(InMemorySpace::new());
    let mind = Arc::new(MockProvider::new(
        "Note down two meeting points, retrieve later",
        8,
    ));

    // One agent: 50-token budget; capabilities = can only write notes/*, can read the whole space
    let mut agent = Agent::new(
        AgentId::new("agent-007"),
        AttentionBudget::new(50, 1000),
        memory.clone(),
        mind,
    )
    .grant(cap(
        "agent-007",
        Permission::Write,
        "mem://agent-007/notes/*",
    ))
    .grant(cap("agent-007", Permission::Read, "mem://agent-007/*"));

    println!("→ THALIOX M1 single-node MVP\n");
    println!(
        "agent: {}  budget: 50 tok  capabilities: Write(notes/*) · Read(*)",
        agent.id()
    );
    agent.start().unwrap();
    println!("[start] phase = {:?}\n", agent.phase());

    // Full pipeline: think → write memory → retrieve
    step(
        &mut agent,
        "think plan",
        Action::Think {
            prompt: "Remember the meeting points".into(),
            cost: 8,
        },
    )
    .await;
    step(
        &mut agent,
        "remember notes/n1",
        Action::Remember {
            object: note("notes/n1", vec![1.0, 0.0, 0.0], "Point A: Q3 roadmap"),
            cost: 5,
        },
    )
    .await;
    step(
        &mut agent,
        "remember notes/n2",
        Action::Remember {
            object: note("notes/n2", vec![0.0, 1.0, 0.0], "Point B: budget review"),
            cost: 5,
        },
    )
    .await;
    step(
        &mut agent,
        "recall [0.9,0.1,0] k=2",
        Action::Recall {
            query: vec![0.9, 0.1, 0.0],
            k: 2,
            cost: 4,
        },
    )
    .await;

    // Gate demo
    println!("\n-- gate demo --");
    step(
        &mut agent,
        "remember secret (out of scope)",
        Action::Remember {
            object: note("secret", vec![0.0, 0.0, 1.0], "not in scope"),
            cost: 5,
        },
    )
    .await;
    step(
        &mut agent,
        "think huge (over budget)",
        Action::Think {
            prompt: "huge task".into(),
            cost: 40,
        },
    )
    .await;

    // INV-4 audit log
    println!("\naudit log (INV-4, {} records):", agent.audit().len());
    for r in agent.audit() {
        let mark = if r.allowed { "✓" } else { "✗" };
        let perm = r
            .permission_used
            .map(|p| format!("{p:?}"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {mark} {:<11?} perm={:<7} cost={:<2} {}",
            r.op, perm, r.cost, r.target
        );
    }

    println!(
        "\n✓ Full pipeline complete: each operation = capability check (INV-2) → budget charge (INV-1) → act on state → audit (INV-4)."
    );
}
