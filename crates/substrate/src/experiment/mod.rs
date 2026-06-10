//! M6 falsification gates (RFC-0008 §7). `e6` is the meter gate — first and
//! non-negotiable: every later M6 gate divides by the baselines it locks.

pub mod e6;
