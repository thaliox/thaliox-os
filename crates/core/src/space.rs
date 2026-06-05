//! The **SemanticSpace** — the TAM "memory": objects addressed by *meaning*,
//! not by a linear address. (TAM §6)

use serde::{Deserialize, Serialize};

use crate::capability::CapabilityToken;
use crate::error::TamError;

/// An object in the semantic space — retrieved by vector similarity, not path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticObject {
    /// Opaque id (a handle, distinct from the vector key).
    pub id: String,
    /// The object's address in semantic space.
    pub vector: Vec<f32>,
    /// Semantic tags.
    pub tags: Vec<String>,
    /// Raw payload the vector stands for.
    pub data: Vec<u8>,
    /// Optional capability guarding access to this object.
    pub capability: Option<CapabilityToken>,
}

/// The contract for a semantic memory: objects are written and recalled by
/// meaning. Concrete stores (in-memory, Qdrant, near-memory silicon) live in
/// `thaliox-memory`; this is the TAM-level interface.
pub trait SemanticSpace {
    /// Store (or replace) an object.
    fn put(&self, obj: SemanticObject) -> Result<(), TamError>;

    /// Fetch a specific object by id.
    fn get(&self, id: &str) -> Result<SemanticObject, TamError>;

    /// Recall the `k` objects nearest to `query` in semantic space.
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<SemanticObject>, TamError>;
}
