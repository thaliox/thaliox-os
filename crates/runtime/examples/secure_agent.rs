//! # secure_agent — capability signature verification (INV-2 / TAM §5.2)
//!
//! For the same "permission + scope", only a capability token that the
//! **issuer actually signed** is accepted by `act`; a token with a forged
//! signature is rejected even if its perm/scope match exactly.
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
    // Issuer's private key; the verifier uses the same key.
    let issuer = HmacSigner::new(b"thaliox-issuer-secret".to_vec());
    let verifier = Arc::new(HmacSigner::new(b"thaliox-issuer-secret".to_vec()));

    // Valid: the issuer signs the token's canonical payload.
    let valid = issuer.issue(cap("agent-x", Permission::Write, "mem://agent-x/*"));
    // Forged: the same grant, but the signature is something an attacker made up.
    let mut forged = cap("agent-x", Permission::Write, "mem://agent-x/*");
    forged.signature = [0x99; 32];

    println!("→ THALIOX capability signature verification (INV-2 / TAM §5.2)\n");

    for (label, token) in [
        ("forged token (fake signature)", forged),
        ("valid token (issuer-signed)", valid),
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
            Ok(_) => println!("· {label:<24} → remember passed ✓"),
            Err(e) => println!("· {label:<24} → denied: {e}"),
        }
    }

    println!(
        "\n✓ Even with identical permission+scope, a capability token with a wrong signature is still rejected by act——\n  verify authenticity before authorization (INV-2); this is exactly the early-prototype lesson TAM §5.2 fixes."
    );
}
