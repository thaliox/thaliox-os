//! # The Model-State Contract (RFC-0002 §4)
//!
//! A model's *recoverable runtime state* is an OS-managed object, not a hidden
//! buffer. [`CognitiveState`] is the seam that makes **M2 snapshot/restore** and
//! **M3 migration/merge** possible for *any* future architecture — the
//! near-term bounded-state hybrid (RFC-0002) and the MELD substrate (RFC-0003)
//! alike — so they stay hot-swappable behind one interface.
//!
//! Scope today is the **state** half of the contract: `serialize` / `restore`
//! (TAM §6 Checkpoint) and `merge` (M3). `merge` defaults to
//! [`StateError::MergeUnsupported`] — it MUST exist in the type system
//! (RFC-0002 C-4), but closing it for real is the mandate of RFC-0003 pillar 2
//! (gated by experiment E1).

use std::error::Error;
use std::fmt;

/// Why a state operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// A blob could not be decoded into this state.
    Decode(String),
    /// `merge` is not implemented for this state yet (RFC-0002 C-4; the gap
    /// RFC-0003 pillar 2 exists to close).
    MergeUnsupported,
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateError::Decode(why) => write!(f, "state decode failed: {why}"),
            StateError::MergeUnsupported => write!(f, "merge is not supported for this state"),
        }
    }
}

impl Error for StateError {}

/// The Model-State Contract: a recoverable runtime state the OS can snapshot,
/// move, and (eventually) merge.
pub trait CognitiveState: Sized {
    /// Freeze to bytes — the basis of a TAM `checkpoint` (RFC-0001 §6).
    fn serialize(&self) -> Vec<u8>;

    /// Rebuild from a blob — the basis of restore, and of migration to another
    /// node (RFC-0001 §6).
    fn restore(blob: &[u8]) -> Result<Self, StateError>;

    /// Merge two diverged states (M3 CRDT merge / self-healing). MUST exist;
    /// MAY be unsupported for now (RFC-0002 C-4). The default refuses — override
    /// it only when a lawful merge exists (RFC-0003 pillar 2, gated by E1).
    fn merge(&self, _other: &Self) -> Result<Self, StateError> {
        Err(StateError::MergeUnsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Counter(u32);

    impl CognitiveState for Counter {
        fn serialize(&self) -> Vec<u8> {
            self.0.to_le_bytes().to_vec()
        }
        fn restore(blob: &[u8]) -> Result<Self, StateError> {
            let arr: [u8; 4] = blob
                .try_into()
                .map_err(|_| StateError::Decode(format!("need 4 bytes, got {}", blob.len())))?;
            Ok(Counter(u32::from_le_bytes(arr)))
        }
    }

    #[test]
    fn serialize_restore_round_trips() {
        let c = Counter(42);
        assert_eq!(Counter::restore(&c.serialize()), Ok(Counter(42)));
    }

    #[test]
    fn restore_rejects_a_bad_blob() {
        assert!(matches!(
            Counter::restore(&[1, 2]),
            Err(StateError::Decode(_))
        ));
    }

    #[test]
    fn merge_defaults_to_unsupported() {
        // RFC-0002 C-4: merge exists in the type system but refuses until MELD.
        assert_eq!(
            Counter(1).merge(&Counter(2)),
            Err(StateError::MergeUnsupported)
        );
    }
}
