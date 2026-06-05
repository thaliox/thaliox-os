//! # THALIOX cap — capability issuing & verification (TAM §5)
//!
//! Implements the two mandated rules from reviewing the earlier prototype:
//!
//! 1. **Scope enforcement** lives in `thaliox-core`
//!    ([`CapabilityToken::authorizes`](thaliox_core::CapabilityToken::authorizes)).
//! 2. **Canonical, length-prefixed signing payloads** live here
//!    ([`canonical_payload`]) — never delimiter-joined, so delimiter injection
//!    cannot forge a colliding signature.
//!
//! M1 status: skeleton — canonical encoder + Issuer/Verifier contracts. The
//! concrete HMAC-SHA256 signer lands next.

use thaliox_core::CapabilityToken;

/// Append `bytes` with a 4-byte little-endian length prefix.
fn push_lp(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

/// **TAM §5.2 rule 2** — canonically encode the signed fields of a token with
/// **length-prefixed** framing. Every variable-length field is length-prefixed
/// so field boundaries are unambiguous; delimiter joining (`|` / `,`) is
/// forbidden because it allows `"ab"+"c"` to collide with `"a"+"bc"`.
///
/// The `signature` field itself is excluded (it signs this payload).
pub fn canonical_payload(tok: &CapabilityToken) -> Vec<u8> {
    let mut b = Vec::new();
    push_lp(&mut b, tok.subject.0.as_bytes());
    b.extend_from_slice(&(tok.permissions.len() as u32).to_le_bytes());
    for p in &tok.permissions {
        b.push(*p as u8);
    }
    b.extend_from_slice(&(tok.scope.len() as u32).to_le_bytes());
    for s in &tok.scope {
        b.push(s.resource as u8);
        push_lp(&mut b, s.pattern.as_bytes());
    }
    b.extend_from_slice(&tok.issued_at.to_le_bytes());
    b.extend_from_slice(&tok.expires_at.to_le_bytes());
    b.extend_from_slice(&tok.jti);
    b.push(tok.delegable as u8);
    b
}

/// Signs canonical payloads (H1: HMAC-SHA256; H3: hardware key).
pub trait Issuer {
    /// Produce a 32-byte signature over `payload`.
    fn sign(&self, payload: &[u8]) -> [u8; 32];
}

/// Verifies signatures over canonical payloads.
pub trait Verifier {
    /// Whether `sig` is a valid signature over `payload`.
    fn verify_signature(&self, payload: &[u8], sig: &[u8; 32]) -> bool;
}

/// Verify a token end to end: recompute its canonical payload and check the
/// signature. (Authorization — INV-2 — is then done via `CapabilityToken::authorizes`.)
pub fn verify_token<V: Verifier>(v: &V, tok: &CapabilityToken) -> bool {
    v.verify_signature(&canonical_payload(tok), &tok.signature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use thaliox_core::{AgentId, Permission, ResourceKind, Scope};

    fn token(subject: &str, pattern: &str) -> CapabilityToken {
        CapabilityToken {
            subject: AgentId::new(subject),
            permissions: vec![Permission::Read],
            scope: vec![Scope {
                resource: ResourceKind::Memory,
                pattern: pattern.into(),
            }],
            issued_at: 1,
            expires_at: 0,
            jti: [7; 16],
            delegable: false,
            signature: [0; 32],
        }
    }

    #[test]
    fn length_prefix_resists_concat_collision() {
        // Naive delimiter-free concatenation collides: "ab"+"c" == "a"+"bc" == "abc".
        // Length-prefixed framing keeps the field boundaries unambiguous.
        let a = token("ab", "c");
        let b = token("a", "bc");
        assert_ne!(canonical_payload(&a), canonical_payload(&b));
    }
}
