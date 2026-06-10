//! # THALIOX substrate (L0) — M6a: meter and enforce below userland
//!
//! H1 built the TAM contract upward; M6 (RFC-0008) starts taking the
//! general-purpose substrate away from under it — **by measurement, never by
//! ideology**. This crate is M6a, the stage every later stage is judged by:
//!
//! - **The substrate ledger** ([`ledger`]) — substrate cost (syscalls, kernel
//!   crossings, context switches, copies) attributed to **TAM operations** and
//!   joined to the INV-4 audit stream: the audit ledger gains a substrate-cost
//!   column, and the H2 efficiency curve is drawn from it and nothing else.
//! - **The probe contract** ([`probe`]) — how cost samples enter the ledger.
//!   CI replays committed captures ([`ReplayProbe`]); the live eBPF probe
//!   (CO-RE tracepoints/kprobes) is the bare-metal leg behind the `ebpf`
//!   feature.
//! - **The E6 meter gate** ([`experiment::e6`]) — the meter itself must cost
//!   < 3% throughput and produce reproducible per-op baselines (< 10% gap
//!   between runs). **No later M6 gate exists until E6 passes**, because every
//!   later gate divides by these numbers.
//! - **The INV-2 deny-floor compiler** ([`policy`]) — each agent's capability
//!   scope compiled to a seccomp profile: an agent whose tokens warrant no
//!   network cannot *make* a connect syscall. A deny-only floor below the
//!   authoritative runtime check, generated from capability state the control
//!   plane already governs — no human writes filter rules (INV-5).

pub mod experiment;
pub mod ledger;
pub mod policy;
pub mod probe;

pub use experiment::e6::{self, E6Report, MAX_BASELINE_GAP, MAX_METER_OVERHEAD};
pub use ledger::{Baseline, LedgerEntry, SubstrateCost, SubstrateEvent, SubstrateLedger};
pub use policy::{DenyFloor, SyscallGroup, compile_deny_floor};
pub use probe::{Probe, ReplayProbe, write_capture};
