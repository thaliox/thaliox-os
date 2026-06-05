//! # 单机 MVP — runtime 驱动一个 agent 跑通全链路
//!
//! cognition → memory → 注意力预算 → 能力校验,每个操作都过 TAM 三重门控
//! (INV-2 能力 · INV-1 预算 · INV-4 审计)。离线可跑(本地 MockProvider)。
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

/// 执行一个动作并打印结果(成功扣预算,失败被门控拒绝)。
async fn step(agent: &mut Agent, label: &str, action: Action) {
    match agent.act(action).await {
        Ok(Outcome::Thought(c)) => println!(
            "· {label:<28} → 思考: \"{}\"  (-{} tok)  余 {}",
            c.content,
            c.tokens,
            agent.remaining_budget()
        ),
        Ok(Outcome::Remembered(id)) => println!(
            "· {label:<28} → 已记忆 '{id}'  余 {}",
            agent.remaining_budget()
        ),
        Ok(Outcome::Recalled(hits)) => {
            let ids: Vec<&str> = hits.iter().map(|o| o.id.as_str()).collect();
            println!(
                "· {label:<28} → 召回 {} 条 {ids:?}  余 {}",
                hits.len(),
                agent.remaining_budget()
            );
        }
        Ok(Outcome::Invoked(out)) => println!(
            "· {label:<28} → 工具输出: {}  余 {}",
            out.chars().take(50).collect::<String>(),
            agent.remaining_budget()
        ),
        Err(e) => println!("· {label:<28} ✗ 拒绝: {e}  余 {}", agent.remaining_budget()),
    }
}

#[tokio::main]
async fn main() {
    // memory(L1)+ cognition(L1,本地离线)
    let memory = Arc::new(InMemorySpace::new());
    let mind = Arc::new(MockProvider::new("记下两条会议要点,稍后检索", 8));

    // 一个 agent:50 token 预算;能力 = 只能写 notes/*、可读全空间
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

    println!("→ THALIOX M1 单机 MVP\n");
    println!(
        "agent: {}  预算: 50 tok  能力: Write(notes/*) · Read(*)",
        agent.id()
    );
    agent.start().unwrap();
    println!("[start] phase = {:?}\n", agent.phase());

    // 全链路:思考 → 写记忆 → 检索
    step(
        &mut agent,
        "think 规划",
        Action::Think {
            prompt: "记住会议要点".into(),
            cost: 8,
        },
    )
    .await;
    step(
        &mut agent,
        "remember notes/n1",
        Action::Remember {
            object: note("notes/n1", vec![1.0, 0.0, 0.0], "要点A:Q3 路线图"),
            cost: 5,
        },
    )
    .await;
    step(
        &mut agent,
        "remember notes/n2",
        Action::Remember {
            object: note("notes/n2", vec![0.0, 1.0, 0.0], "要点B:预算评审"),
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

    // 门控演示
    println!("\n-- 门控演示 --");
    step(
        &mut agent,
        "remember secret (越权)",
        Action::Remember {
            object: note("secret", vec![0.0, 0.0, 1.0], "不在 scope"),
            cost: 5,
        },
    )
    .await;
    step(
        &mut agent,
        "think 巨型 (超预算)",
        Action::Think {
            prompt: "超大任务".into(),
            cost: 40,
        },
    )
    .await;

    // INV-4 审计日志
    println!("\n审计日志 (INV-4,共 {} 条):", agent.audit().len());
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
        "\n✓ 全链路跑通:每个操作 = 能力校验(INV-2) → 预算扣减(INV-1) → 作用于状态 → 审计(INV-4)。"
    );
}
