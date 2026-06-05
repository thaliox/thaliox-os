//! # secure_agent — 能力签名校验(INV-2 / TAM §5.2)
//!
//! 同一份「权限 + 作用域」,只有**签发者真正签过名**的能力令牌才被 `act` 接受;
//! 伪造签名的令牌即使 perm/scope 完全匹配也被拒。
//!
//! ```bash
//! cargo run -p thaliox-runtime --example secure_agent
//! ```

use std::sync::Arc;

use thaliox_cap::HmacSigner;
use thaliox_cognition::MockProvider;
use thaliox_core::{
    AgentId, AttentionBudget, CapabilityToken, Permission, ResourceKind, Scope, SemanticObject,
};
use thaliox_memory::InMemorySpace;
use thaliox_runtime::{Action, Agent};

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

fn note() -> SemanticObject {
    SemanticObject {
        id: "n1".into(),
        vector: vec![1.0, 0.0],
        tags: vec![],
        data: b"hello".to_vec(),
        capability: None,
    }
}

#[tokio::main]
async fn main() {
    // 签发者私钥;验证器用同一密钥。
    let issuer = HmacSigner::new(b"thaliox-issuer-secret".to_vec());
    let verifier = Arc::new(HmacSigner::new(b"thaliox-issuer-secret".to_vec()));

    // 合法:签发者对令牌规范化负载签名。
    let valid = issuer.issue(cap("agent-x", Permission::Write, "mem://agent-x/*"));
    // 伪造:相同 grant,但签名是攻击者瞎填的。
    let mut forged = cap("agent-x", Permission::Write, "mem://agent-x/*");
    forged.signature = [0x99; 32];

    println!("→ THALIOX 能力签名校验(INV-2 / TAM §5.2)\n");

    for (label, token) in [
        ("伪造令牌(假签名)", forged),
        ("合法令牌(签发者签发)", valid),
    ] {
        let mut a = Agent::new(
            AgentId::new("agent-x"),
            AttentionBudget::new(100, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        )
        .with_verifier(verifier.clone())
        .grant(token);
        a.start().unwrap();
        match a
            .act(Action::Remember {
                object: note(),
                cost: 3,
            })
            .await
        {
            Ok(_) => println!("· {label:<24} → remember 通过 ✓"),
            Err(e) => println!("· {label:<24} → 拒绝: {e}"),
        }
    }

    println!(
        "\n✓ 权限+作用域完全相同,签名不对的能力令牌仍被 act 拒绝——\n  鉴权前先验真伪(INV-2),这正是 TAM §5.2 要修正的早期原型教训。"
    );
}
