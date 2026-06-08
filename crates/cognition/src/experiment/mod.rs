//! # RFC-0003 falsification experiments
//!
//! Toy-scale, zero-dependency, deterministic gates for the MELD pillars
//! (RFC-0003 §5). Each experiment **isolates one primitive** and reports a
//! pass/kill verdict — evidence, not ambition, advances the moonshot.
//!
//! - [`e1`] — **pillar 2**, Mergeable Cognition: can a fixed-size latent admit a
//!   merge operator that is both *useful* and *CRDT-lawful*?
//! - [`e2`] — **pillar 3**, Energy-based Latent Readout: does iterative energy
//!   minimization give a monotone, saturating *steps↔quality* curve — i.e. a real
//!   AttentionBudget knob (RFC-0001 §4; RFC-0002 §3.3)?

pub mod e1;
pub mod e2;

pub use e1::run_e1;
pub use e2::run_e2;
