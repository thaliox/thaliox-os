//! # RFC-0003 falsification experiments (dataflow pillar)
//!
//! Toy-scale, deterministic gates for MELD's execution pillar (RFC-0003 §5),
//! living next to the [`Scheduler`](crate::Scheduler) interface they motivate.
//!
//! - `e4` — **pillar 5**, Dataflow Execution: can a forward "pass" be expressed
//!   as a dependency graph, partitioned across ≥2 nodes, and scheduled so the
//!   result is **location-independent** (identical to a monolithic run) while
//!   actually **overlapping** independent work?

pub mod e4;

pub use e4::run_e4;
