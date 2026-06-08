//! # E3 — Capability-addressed Memory (RFC-0003 §5)
//!
//! Falsification gate for **MELD pillar 4 — Capability-addressed Memory**
//! (RFC-0003 §4): every memory access is gated by an unforgeable capability such
//! that *without one the data is **structurally unreachable**, not merely
//! refused*. The distinction is the whole point and is easy to blur:
//!
//! - **Checked** (weak): plaintext sits in a map keyed by logical name; `read`
//!   guards it with `if authorizes { .. } else { Err }`. One missing check — or
//!   a leak of the persisted state — and the plaintext is gone. Refusal, not
//!   structure.
//! - **Addressed** (strong): entries live under an opaque tag `MAC(secret,
//!   target)` with plaintext **locked** by `MAC(secret, target)`. The *only*
//!   read path derives the tag and unlock key **after** the capability verifies
//!   and authorizes; the persisted map carries **no logical names and no
//!   plaintext**, so even exfiltrating it whole reveals nothing without the
//!   runtime secret.
//!
//! The decisive question E3 settles at toy scale:
//!
//! > For the addressed store, is there **any** path — bad cap, wrong scope,
//! > forged or expired token, or a full raw dump — that yields one plaintext
//! > byte? If not (and the authorized read still works), access is structural.
//!
//! E3 exercises the **real** capability stack: tokens are signed and verified by
//! [`HmacSigner`](crate::HmacSigner) and gated by
//! [`CapabilityToken::authorizes`]. It runs the same adversary battery against
//! both stores and reports the contrast: the checked store leaks on a raw dump;
//! the addressed store does not.
//!
//! **Honest scope.** E3 proves the *necessary* property (no plaintext without a
//! valid authorizing capability, on any software path — including memory
//! disclosure). It does **not** prove the *sufficient* H3 guarantee where the
//! capability *is* the hardware-enforced pointer (CHERI tags); that is the
//! silicon claim E3's software model only motivates.
//!
//! Run it: `cargo run -p thaliox-cap --example e3_capability_addressed`.

use std::collections::HashMap;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use thaliox_core::{AgentId, CapabilityToken, CapabilityVerifier, Permission, ResourceKind, Scope};

use crate::HmacSigner;

type H = Hmac<Sha256>;

/// A keyed PRF over `(label, target)` with unambiguous, length-prefixed framing.
fn prf(secret: &[u8], label: &[u8], target: &[u8]) -> [u8; 32] {
    let mut mac = H::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&(label.len() as u32).to_le_bytes());
    mac.update(label);
    mac.update(&(target.len() as u32).to_le_bytes());
    mac.update(target);
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

/// Why an access was denied — all of these must leave the data unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// Signature invalid or token expired (forged / stale).
    Unauthentic,
    /// Authentic, but permission class or scope does not cover the target.
    OutOfScope,
    /// No such object.
    Absent,
}

/// A store holding a secret, accessed under a capability, plus the adversary's
/// view: a full dump of persisted state.
trait SecretStore {
    fn read(
        &self,
        verifier: &HmacSigner,
        cap: &CapabilityToken,
        target: &str,
        now: u64,
    ) -> Result<Vec<u8>, Denied>;

    /// Does the *persisted state*, dumped whole, contain `needle` anywhere?
    /// This is the structural test: exfiltrating the store must reveal nothing.
    fn leaks_via_dump(&self, needle: &[u8]) -> bool;
}

/// WEAK model — plaintext keyed by logical name, guarded by a runtime check.
#[derive(Default)]
pub struct CheckedMemory {
    store: HashMap<String, Vec<u8>>,
}

impl CheckedMemory {
    pub fn write(&mut self, target: &str, plaintext: &[u8]) {
        self.store.insert(target.to_string(), plaintext.to_vec());
    }
}

impl SecretStore for CheckedMemory {
    fn read(
        &self,
        verifier: &HmacSigner,
        cap: &CapabilityToken,
        target: &str,
        now: u64,
    ) -> Result<Vec<u8>, Denied> {
        if !verifier.verify(cap, now) {
            return Err(Denied::Unauthentic);
        }
        if !cap.authorizes(Permission::Read, ResourceKind::Memory, target) {
            return Err(Denied::OutOfScope);
        }
        self.store.get(target).cloned().ok_or(Denied::Absent)
    }

    fn leaks_via_dump(&self, needle: &[u8]) -> bool {
        // Plaintext sits right here, keyed by name — the dump leaks it.
        self.store.values().any(|v| contains(v, needle))
    }
}

/// STRONG model — opaque tag → locked blob; no names, no plaintext at rest.
pub struct AddressedMemory {
    /// Held by the runtime only. Not derivable from any capability.
    secret: Vec<u8>,
    store: HashMap<[u8; 32], Vec<u8>>,
}

impl AddressedMemory {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            secret: secret.into(),
            store: HashMap::new(),
        }
    }

    fn tag(&self, target: &str) -> [u8; 32] {
        prf(&self.secret, b"addr", target.as_bytes())
    }

    /// A pseudo-random keystream of length `n`, bound to `target`.
    fn keystream(&self, target: &str, n: usize) -> Vec<u8> {
        let mut ks = Vec::with_capacity(n);
        let mut ctr: u32 = 0;
        while ks.len() < n {
            let mut block_input = target.as_bytes().to_vec();
            block_input.extend_from_slice(&ctr.to_le_bytes());
            ks.extend_from_slice(&prf(&self.secret, b"lock", &block_input));
            ctr += 1;
        }
        ks.truncate(n);
        ks
    }

    pub fn write(&mut self, target: &str, plaintext: &[u8]) {
        let ks = self.keystream(target, plaintext.len());
        let blob: Vec<u8> = plaintext.iter().zip(&ks).map(|(p, k)| p ^ k).collect();
        self.store.insert(self.tag(target), blob);
    }
}

impl SecretStore for AddressedMemory {
    fn read(
        &self,
        verifier: &HmacSigner,
        cap: &CapabilityToken,
        target: &str,
        now: u64,
    ) -> Result<Vec<u8>, Denied> {
        // Authentic and authorizing BEFORE any address is even formed.
        if !verifier.verify(cap, now) {
            return Err(Denied::Unauthentic);
        }
        if !cap.authorizes(Permission::Read, ResourceKind::Memory, target) {
            return Err(Denied::OutOfScope);
        }
        // Only now derive the address and the unlock keystream.
        let blob = self.store.get(&self.tag(target)).ok_or(Denied::Absent)?;
        let ks = self.keystream(target, blob.len());
        Ok(blob.iter().zip(&ks).map(|(b, k)| b ^ k).collect())
    }

    fn leaks_via_dump(&self, needle: &[u8]) -> bool {
        // Tags are MACs; blobs are locked. Without `secret`, nothing maps back.
        self.store.values().any(|v| contains(v, needle))
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// One adversarial attempt and whether it leaked the secret.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub name: &'static str,
    pub leaked: bool,
    pub note: &'static str,
}

/// Battery outcome for one store.
#[derive(Debug, Clone)]
pub struct MemReport {
    pub store: &'static str,
    /// The legitimate, authorized read returns the correct plaintext.
    pub authorized_ok: bool,
    pub attempts: Vec<Attempt>,
}

impl MemReport {
    /// Structural iff the authorized read works AND no unauthorized path leaks.
    pub fn structural(&self) -> bool {
        self.authorized_ok && self.attempts.iter().all(|a| !a.leaked)
    }
}

/// Full E3 report — the verdict for pillar 4, with the checked-store contrast.
#[derive(Debug, Clone)]
pub struct E3Report {
    pub addressed: MemReport,
    pub checked: MemReport,
}

impl E3Report {
    /// Gate (RFC-0003 §5): the addressed store is structural.
    /// `false` ⇒ kill / redesign pillar 4.
    pub fn structural(&self) -> bool {
        self.addressed.structural()
    }

    /// The contrast proving the distinction is real, not cosmetic: the checked
    /// store *does* leak (on a raw dump), so "addressed" is necessary.
    pub fn checked_leaks(&self) -> bool {
        !self.checked.structural()
    }
}

fn mk_cap(
    signer: &HmacSigner,
    permissions: Vec<Permission>,
    pattern: &str,
    expires_at: u64,
) -> CapabilityToken {
    signer.issue(CapabilityToken {
        subject: AgentId::new("a1"),
        permissions,
        scope: vec![Scope {
            resource: ResourceKind::Memory,
            pattern: pattern.to_string(),
        }],
        issued_at: 1,
        expires_at,
        jti: [1; 16],
        delegable: false,
        signature: [0; 32],
    })
}

/// Run the adversary battery against `store`, returning its report.
fn run_battery<S: SecretStore>(store: &S, name: &'static str, issuer: &HmacSigner) -> MemReport {
    const SECRET: &[u8] = b"launch-codes-7731";
    let target = "mem://team-a/notes";
    let now = 50;

    // The legitimate holder: Read over a scope that matches, validly signed.
    let good = mk_cap(issuer, vec![Permission::Read], "mem://team-a/*", 0);
    let authorized_ok = store.read(issuer, &good, target, now).as_deref() == Ok(SECRET);

    // Each unauthorized path must NOT yield the secret.
    let leaked = |r: Result<Vec<u8>, Denied>| matches!(r, Ok(d) if d == SECRET);

    let wrong_scope = mk_cap(issuer, vec![Permission::Read], "mem://team-b/*", 0);
    let no_read = mk_cap(issuer, vec![Permission::Write], "mem://team-a/*", 0);
    let expired = mk_cap(issuer, vec![Permission::Read], "mem://team-a/*", 10);
    // Forged: an attacker self-signs a cap that *claims* the right scope.
    let attacker = HmacSigner::new(b"attacker-guess".to_vec());
    let forged = mk_cap(&attacker, vec![Permission::Read], "mem://team-a/*", 0);

    let attempts = vec![
        Attempt {
            name: "raw dump (no cap)",
            leaked: store.leaks_via_dump(SECRET),
            note: "exfiltrate the whole persisted store",
        },
        Attempt {
            name: "wrong-scope cap",
            leaked: leaked(store.read(issuer, &wrong_scope, target, now)),
            note: "Read, but scope is team-b",
        },
        Attempt {
            name: "missing Read perm",
            leaked: leaked(store.read(issuer, &no_read, target, now)),
            note: "Write-only over the right scope",
        },
        Attempt {
            name: "expired cap",
            leaked: leaked(store.read(issuer, &expired, target, now)),
            note: "valid signature, past expiry",
        },
        Attempt {
            name: "forged cap",
            leaked: leaked(store.read(issuer, &forged, target, now)),
            note: "attacker self-signed the scope",
        },
    ];

    MemReport {
        store: name,
        authorized_ok,
        attempts,
    }
}

/// Run E3 deterministically.
pub fn run_e3() -> E3Report {
    const SECRET: &[u8] = b"launch-codes-7731";
    let target = "mem://team-a/notes";
    let issuer = HmacSigner::new(b"runtime-root-key".to_vec());

    let mut addressed = AddressedMemory::new(b"store-master-secret".to_vec());
    addressed.write(target, SECRET);

    let mut checked = CheckedMemory::default();
    checked.write(target, SECRET);

    E3Report {
        addressed: run_battery(&addressed, "AddressedMemory", &issuer),
        checked: run_battery(&checked, "CheckedMemory", &issuer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorized_read_works() {
        assert!(run_e3().addressed.authorized_ok);
    }

    #[test]
    fn no_unauthorized_path_leaks_from_addressed() {
        for a in run_e3().addressed.attempts {
            assert!(!a.leaked, "addressed leaked via {}: {}", a.name, a.note);
        }
    }

    #[test]
    fn addressed_store_is_structural() {
        assert!(run_e3().structural());
    }

    #[test]
    fn checked_store_leaks_on_raw_dump() {
        // The contrast: a plain checked store is refusal, not structure.
        let r = run_e3();
        assert!(r.checked_leaks());
        let dump = r
            .checked
            .attempts
            .iter()
            .find(|a| a.name == "raw dump (no cap)")
            .unwrap();
        assert!(
            dump.leaked,
            "checked store should leak its plaintext on dump"
        );
    }

    #[test]
    fn forged_and_expired_are_denied() {
        let r = run_e3().addressed;
        for name in ["forged cap", "expired cap"] {
            let a = r.attempts.iter().find(|a| a.name == name).unwrap();
            assert!(!a.leaked);
        }
    }

    #[test]
    fn deterministic_across_runs() {
        assert_eq!(run_e3().structural(), run_e3().structural());
    }
}
