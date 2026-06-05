//! Primitive #1: the **vector message** — the unit of *meaning* agents exchange,
//! not a byte stream. (TAM §3)

use serde::{Deserialize, Serialize};

use crate::agent::AgentId;
use crate::capability::CapabilityToken;

/// A unit of meaning exchanged between agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorMessage {
    /// Sender.
    pub from: AgentId,
    /// Unicast recipient or a multicast intent group.
    pub to: Recipient,
    /// The sender's vector space.
    pub fingerprint: ModelFingerprint,
    /// Message class.
    pub kind: MessageKind,
    /// The payload (dense / sparse / raw escape hatch).
    pub payload: VectorPayload,
    /// Optional intent vector for semantic routing.
    pub intent: Option<IntentVector>,
    /// Stream fragment sequence number.
    pub seq: u64,
    /// Authorization for cross-agent operations.
    pub capability: Option<CapabilityToken>,
}

/// Identifies the embedding space a vector belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFingerprint {
    pub model_id: String,
    pub revision: String,
    pub dim: u32,
}

impl ModelFingerprint {
    /// **INV-3**: equal fingerprints may inject payloads with zero loss; unequal
    /// fingerprints require explicit, measurable translation.
    pub fn compatible_with(&self, other: &ModelFingerprint) -> bool {
        self == other
    }
}

/// Unicast or multicast addressing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Recipient {
    Unicast(AgentId),
    Multicast(IntentGroup),
}

/// A multicast group addressed by shared intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentGroup(pub String);

/// Message class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    Data,
    Intent,
    Translate,
    Control,
}

/// The message payload. `Raw` is the interop escape hatch — TAM treats it as
/// *unaligned*, so it does NOT enjoy the zero-loss guarantee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VectorPayload {
    /// Row-major dense vector.
    Dense {
        dtype: Dtype,
        dim: u32,
        data: Vec<u8>,
    },
    /// Sparse vector.
    Sparse {
        dim: u32,
        indices: Vec<u32>,
        values: Vec<u8>,
    },
    /// Escape hatch: text / JSON for interop with the outside world.
    Raw {
        content_type: String,
        bytes: Vec<u8>,
    },
}

/// Wire dtypes for vector payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dtype {
    Fp32,
    Fp16,
    Bf16,
    Fp8E4,
    Fp8E5,
    Int8,
}

/// An optional intent vector used for semantic routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentVector {
    pub dim: u32,
    pub data: Vec<f32>,
}
