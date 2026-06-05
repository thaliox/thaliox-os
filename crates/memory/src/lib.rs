//! # THALIOX memory (L1)
//!
//! The [`SemanticSpace`] (TAM §6) plus the four
//! memory tiers: **working** (context / KV-cache), **episodic** (recent
//! sessions, time-windowed), **semantic** (long-term knowledge, persistent
//! vectors), **procedural** (skill / tool-use patterns).
//!
//! M1 status: skeleton — an in-memory `SemanticSpace` (brute-force cosine) so
//! the contract is exercised end to end; Qdrant / LanceDB / near-memory silicon
//! back ends slot in behind the same trait later.

use parking_lot::RwLock;
use std::collections::HashMap;
use thaliox_core::{SemanticObject, SemanticSpace, TamError};

/// The four memory tiers (MASTER_PLAN §1.3, TAM §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Working,
    Episodic,
    Semantic,
    Procedural,
}

/// A minimal in-memory semantic space: brute-force cosine recall. Reference
/// implementation of the TAM contract; production back ends replace it.
#[derive(Default)]
pub struct InMemorySpace {
    objects: RwLock<HashMap<String, SemanticObject>>,
}

impl InMemorySpace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.objects.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.read().is_empty()
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

impl SemanticSpace for InMemorySpace {
    fn put(&self, obj: SemanticObject) -> Result<(), TamError> {
        self.objects.write().insert(obj.id.clone(), obj);
        Ok(())
    }

    fn get(&self, id: &str) -> Result<SemanticObject, TamError> {
        self.objects
            .read()
            .get(id)
            .cloned()
            .ok_or_else(|| TamError::NotFound(id.to_string()))
    }

    fn search(&self, query: &[f32], k: usize) -> Result<Vec<SemanticObject>, TamError> {
        let objs = self.objects.read();
        let mut scored: Vec<(f32, &SemanticObject)> = objs
            .values()
            .map(|o| (cosine(query, &o.vector), o))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(k).map(|(_, o)| o.clone()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(id: &str, v: Vec<f32>) -> SemanticObject {
        SemanticObject {
            id: id.into(),
            vector: v,
            tags: vec![],
            data: vec![],
            capability: None,
        }
    }

    #[test]
    fn recall_by_meaning() {
        let s = InMemorySpace::new();
        s.put(obj("a", vec![1.0, 0.0, 0.0])).unwrap();
        s.put(obj("b", vec![0.0, 1.0, 0.0])).unwrap();
        let hits = s.search(&[0.9, 0.1, 0.0], 1).unwrap();
        assert_eq!(hits[0].id, "a");
        assert_eq!(s.get("b").unwrap().id, "b");
        assert!(s.get("missing").is_err());
    }
}
