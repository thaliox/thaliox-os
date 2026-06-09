//! # RFC-0003 falsification experiments (capability pillar)
//!
//! Toy-scale, deterministic gates for MELD's capability-side pillars
//! (RFC-0003 §5), living next to the real `cap` primitives they exercise — E3
//! runs the *production* `HmacSigner` verify and `CapabilityToken::authorizes`,
//! not a reimplementation.
//!
//! - `e3` — **pillar 4**, Capability-addressed Memory: is unauthorized access
//!   *structurally impossible* (no plaintext on any path, including a full raw
//!   dump of persisted state) rather than merely refused by an `if`?

pub mod e3;

pub use e3::run_e3;
