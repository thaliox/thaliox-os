//! # THALIOX fabric (L3)
//!
//! agent↔agent communication, team orchestration, and CRDT state replication
//! (MASTER_PLAN §2). Transport carries [`VectorMessage`]s (TAM §3) near-term
//! over gRPC/QUIC, long-term the `vsend`/`vrecv` hardware
//! primitive. A **team** is a *holarchy* — agents that are whole yet compose
//! into a larger whole.
//!
//! M4a: a real in-process [`LocalFabric`] routes [`VectorMessage`]s between
//! [`Endpoint`]s, enforcing INV-2 (capability-gated) on send and INV-3 (vector
//! fidelity, via [`fidelity`]) on injection.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use thaliox_core::{
    AgentId, ModelFingerprint, Permission, Recipient, ResourceKind, TamError, VectorMessage,
    VectorPayload,
};

/// Carries vector messages between agents.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a vector message (unicast or multicast to an intent group).
    async fn send(&self, msg: VectorMessage) -> Result<(), TamError>;

    /// Receive the next inbound vector message.
    async fn recv(&self) -> Result<VectorMessage, TamError>;
}

/// Composable collaboration paradigms a team may adopt (MASTER_PLAN §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paradigm {
    Hierarchy,
    Market,
    Swarm,
    Pipeline,
}

/// A team: a holarchy of agents with shared goals and assigned roles.
#[derive(Debug, Clone)]
pub struct Team {
    pub name: String,
    pub members: Vec<AgentId>,
    pub paradigm: Paradigm,
}

// ---------- M4a: in-process fabric (RFC-0006 §2) ----------

/// A shared, in-process message fabric. Routes [`VectorMessage`]s between agent
/// [`Endpoint`]s by recipient (unicast or multicast intent group). Cloneable —
/// every endpoint shares the same routing state.
#[derive(Clone, Default)]
pub struct LocalFabric {
    inboxes: Arc<Mutex<HashMap<AgentId, VecDeque<VectorMessage>>>>,
    groups: Arc<Mutex<HashMap<String, HashSet<AgentId>>>>,
}

impl LocalFabric {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent and get its endpoint. `fingerprint` is the agent's
    /// vector space, used for INV-3 fidelity checks on what it receives.
    pub fn endpoint(&self, id: AgentId, fingerprint: ModelFingerprint) -> Endpoint {
        self.inboxes.lock().unwrap().entry(id.clone()).or_default();
        Endpoint {
            id,
            fingerprint,
            fabric: self.clone(),
        }
    }

    /// Add an agent to a multicast intent group.
    pub fn join(&self, id: &AgentId, group: &str) {
        self.groups
            .lock()
            .unwrap()
            .entry(group.to_string())
            .or_default()
            .insert(id.clone());
    }

    fn deliver(&self, to: &AgentId, msg: VectorMessage) {
        self.inboxes
            .lock()
            .unwrap()
            .entry(to.clone())
            .or_default()
            .push_back(msg);
    }

    fn group_members(&self, group: &str) -> Vec<AgentId> {
        self.groups
            .lock()
            .unwrap()
            .get(group)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// One agent's handle on the fabric.
pub struct Endpoint {
    id: AgentId,
    fingerprint: ModelFingerprint,
    fabric: LocalFabric,
}

impl Endpoint {
    pub fn id(&self) -> &AgentId {
        &self.id
    }
    pub fn fingerprint(&self) -> &ModelFingerprint {
        &self.fingerprint
    }
}

#[async_trait]
impl Transport for Endpoint {
    async fn send(&self, msg: VectorMessage) -> Result<(), TamError> {
        // INV-2: a cross-agent send MUST carry a capability granting `Communicate`
        // over the recipient (an agent id or an intent group).
        let target = match &msg.to {
            Recipient::Unicast(id) => id.0.clone(),
            Recipient::Multicast(g) => g.0.clone(),
        };
        let authorized = msg
            .capability
            .as_ref()
            .is_some_and(|c| c.authorizes(Permission::Communicate, ResourceKind::Agent, &target));
        if !authorized {
            return Err(TamError::CapabilityDenied(format!(
                "Communicate on {target}"
            )));
        }

        match msg.to.clone() {
            Recipient::Unicast(id) => self.fabric.deliver(&id, msg),
            Recipient::Multicast(g) => {
                for m in self.fabric.group_members(&g.0) {
                    self.fabric.deliver(&m, msg.clone());
                }
            }
        }
        Ok(())
    }

    async fn recv(&self) -> Result<VectorMessage, TamError> {
        self.fabric
            .inboxes
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .and_then(|q| q.pop_front())
            .ok_or_else(|| TamError::Invalid(format!("no message for {}", self.id)))
    }
}

/// INV-3 verdict: whether a received message can be injected into the receiver's
/// model directly, must be translated first, or is an unaligned escape hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fidelity {
    /// Same fingerprint, aligned vector — inject directly, zero loss.
    Lossless,
    /// Different fingerprint, aligned vector — MUST be explicitly translated.
    NeedsTranslation,
    /// `Raw` payload — unaligned interop; no zero-loss guarantee.
    Unaligned,
}

/// **INV-3 (vector fidelity)**: classify `msg` for a receiver with `receiver`'s
/// fingerprint. Forbids *implicit* lossy cross-fingerprint injection — a
/// mismatched aligned payload is `NeedsTranslation`, never silently `Lossless`.
pub fn fidelity(msg: &VectorMessage, receiver: &ModelFingerprint) -> Fidelity {
    match &msg.payload {
        VectorPayload::Raw { .. } => Fidelity::Unaligned,
        _ if msg.fingerprint.compatible_with(receiver) => Fidelity::Lossless,
        _ => Fidelity::NeedsTranslation,
    }
}

#[cfg(test)]
mod tests {
    use thaliox_core::{CapabilityToken, Dtype, IntentGroup, MessageKind, Scope};

    use super::*;

    fn fp(model: &str) -> ModelFingerprint {
        ModelFingerprint {
            model_id: model.into(),
            revision: "1".into(),
            dim: 4,
        }
    }

    fn comm_cap(subject: &str, pattern: &str) -> CapabilityToken {
        CapabilityToken {
            subject: AgentId::new(subject),
            permissions: vec![Permission::Communicate],
            scope: vec![Scope {
                resource: ResourceKind::Agent,
                pattern: pattern.into(),
            }],
            issued_at: 0,
            expires_at: 0,
            jti: [0; 16],
            delegable: false,
            signature: [0; 32],
        }
    }

    fn msg(from: &str, to: Recipient, model: &str, cap: Option<CapabilityToken>) -> VectorMessage {
        VectorMessage {
            from: AgentId::new(from),
            to,
            fingerprint: fp(model),
            kind: MessageKind::Data,
            payload: VectorPayload::Dense {
                dtype: Dtype::Fp32,
                dim: 4,
                data: vec![0; 16],
            },
            intent: None,
            seq: 0,
            capability: cap,
        }
    }

    #[tokio::test]
    async fn unicast_delivers_with_capability() {
        let fabric = LocalFabric::new();
        let a = fabric.endpoint(AgentId::new("a"), fp("m1"));
        let b = fabric.endpoint(AgentId::new("b"), fp("m1"));
        let m = msg(
            "a",
            Recipient::Unicast(AgentId::new("b")),
            "m1",
            Some(comm_cap("a", "b")),
        );
        a.send(m).await.unwrap();
        assert_eq!(b.recv().await.unwrap().from, AgentId::new("a"));
    }

    #[tokio::test]
    async fn send_without_capability_is_denied() {
        let fabric = LocalFabric::new();
        let a = fabric.endpoint(AgentId::new("a"), fp("m1"));
        let _b = fabric.endpoint(AgentId::new("b"), fp("m1"));
        let r = a
            .send(msg("a", Recipient::Unicast(AgentId::new("b")), "m1", None))
            .await;
        assert!(matches!(r, Err(TamError::CapabilityDenied(_))));
    }

    #[tokio::test]
    async fn wrong_scope_capability_is_denied() {
        let fabric = LocalFabric::new();
        let a = fabric.endpoint(AgentId::new("a"), fp("m1"));
        // Cap authorizes talking to "c", but the message targets "b".
        let r = a
            .send(msg(
                "a",
                Recipient::Unicast(AgentId::new("b")),
                "m1",
                Some(comm_cap("a", "c")),
            ))
            .await;
        assert!(matches!(r, Err(TamError::CapabilityDenied(_))));
    }

    #[tokio::test]
    async fn multicast_delivers_to_group() {
        let fabric = LocalFabric::new();
        let a = fabric.endpoint(AgentId::new("a"), fp("m1"));
        let b = fabric.endpoint(AgentId::new("b"), fp("m1"));
        let c = fabric.endpoint(AgentId::new("c"), fp("m1"));
        fabric.join(&AgentId::new("b"), "team");
        fabric.join(&AgentId::new("c"), "team");
        a.send(msg(
            "a",
            Recipient::Multicast(IntentGroup("team".into())),
            "m1",
            Some(comm_cap("a", "team")),
        ))
        .await
        .unwrap();
        assert!(b.recv().await.is_ok());
        assert!(c.recv().await.is_ok());
    }

    #[test]
    fn inv3_fidelity_classification() {
        let receiver = fp("m1");
        // same fingerprint, aligned → lossless
        let same = msg("a", Recipient::Unicast(AgentId::new("b")), "m1", None);
        assert_eq!(fidelity(&same, &receiver), Fidelity::Lossless);
        // different fingerprint, aligned → needs translation (no implicit inject)
        let diff = msg("a", Recipient::Unicast(AgentId::new("b")), "m2", None);
        assert_eq!(fidelity(&diff, &receiver), Fidelity::NeedsTranslation);
        // raw payload → unaligned escape hatch
        let mut raw = diff.clone();
        raw.payload = VectorPayload::Raw {
            content_type: "text/plain".into(),
            bytes: b"hi".to_vec(),
        };
        assert_eq!(fidelity(&raw, &receiver), Fidelity::Unaligned);
    }
}
