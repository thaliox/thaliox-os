//! # E6 — the meter gate (RFC-0008 §7, M6a)
//!
//! Before the substrate ledger may judge anything, it must pass judgment on
//! itself, twice over:
//!
//! 1. **Overhead** — running the probes must cost the workload < 3% wall time
//!    (a meter that distorts what it measures draws a curve of itself);
//! 2. **Reproducibility** — per-op baselines from independent runs of the same
//!    workload must agree within 10% on every metric (a number that won't
//!    reproduce is not a baseline, and every later gate divides by it).
//!
//! The harness is pure logic: in CI it runs over committed captures recorded
//! on real hardware (replay mode); the live leg records those captures on the
//! bare-metal box. **E6 has no verdict until live captures exist** — synthetic
//! data exercises the harness in tests, it never passes the gate.

use crate::ledger::Baseline;

/// Ceiling on the meter's own cost: `(metered − unmetered) / unmetered`.
pub const MAX_METER_OVERHEAD: f64 = 0.03;

/// Ceiling on the worst per-op, per-metric relative gap between runs.
pub const MAX_BASELINE_GAP: f64 = 0.10;

/// The E6 verdict.
#[derive(Debug, Clone)]
pub struct E6Report {
    /// Fractional wall-time cost of running the probes.
    pub overhead: f64,
    /// Worst relative gap between any two runs' baselines for the same op.
    pub max_gap: f64,
    /// Ops compared across runs.
    pub ops_compared: usize,
    pub passed: bool,
}

/// Evaluate E6 from a workload timed without (`unmetered_ns`) and with
/// (`metered_ns`) the probes attached, plus per-op [`Baseline`]s from two or
/// more independent metered runs.
pub fn evaluate(unmetered_ns: u64, metered_ns: u64, runs: &[Vec<Baseline>]) -> E6Report {
    let overhead = if unmetered_ns == 0 {
        f64::INFINITY
    } else {
        (metered_ns as f64 - unmetered_ns as f64) / unmetered_ns as f64
    };

    let mut max_gap = 0.0f64;
    let mut ops_compared = 0usize;
    if let Some((first, rest)) = runs.split_first() {
        for baseline in first {
            for other_run in rest {
                match other_run.iter().find(|b| b.op == baseline.op) {
                    Some(other) => {
                        max_gap = max_gap.max(baseline.relative_gap(other));
                        ops_compared += 1;
                    }
                    // An op observed in one run but not another is itself a
                    // reproducibility failure.
                    None => max_gap = f64::INFINITY,
                }
            }
        }
    }

    // Fewer than two runs ⇒ nothing was reproduced ⇒ no pass.
    let reproducible = runs.len() >= 2 && max_gap <= MAX_BASELINE_GAP;
    E6Report {
        overhead,
        max_gap,
        ops_compared,
        passed: overhead <= MAX_METER_OVERHEAD && reproducible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thaliox_core::Operation;

    fn baseline(op: Operation, syscalls: f64) -> Baseline {
        Baseline {
            op,
            n: 100,
            mean: [syscalls, syscalls * 2.0, 1.0, 1e6, 4096.0],
        }
    }

    fn run(tool: f64, vsend: f64) -> Vec<Baseline> {
        vec![
            baseline(Operation::ToolInvoke, tool),
            baseline(Operation::VSend, vsend),
        ]
    }

    #[test]
    fn tight_runs_and_cheap_meter_pass() {
        let runs = vec![run(100.0, 10.0), run(103.0, 10.4)]; // ≤4% gaps
        let r = evaluate(1_000_000, 1_020_000, &runs); // 2% overhead
        assert!(r.passed, "{r:?}");
        assert!(r.overhead < MAX_METER_OVERHEAD);
        assert!(r.max_gap < MAX_BASELINE_GAP);
        assert_eq!(r.ops_compared, 2);
    }

    #[test]
    fn an_expensive_meter_fails_regardless_of_reproducibility() {
        let runs = vec![run(100.0, 10.0), run(100.0, 10.0)];
        let r = evaluate(1_000_000, 1_080_000, &runs); // 8% overhead
        assert!(!r.passed);
        assert!(r.overhead > MAX_METER_OVERHEAD);
    }

    #[test]
    fn irreproducible_baselines_fail_regardless_of_overhead() {
        let runs = vec![run(100.0, 10.0), run(130.0, 10.0)]; // 23% gap on ToolInvoke
        let r = evaluate(1_000_000, 1_010_000, &runs);
        assert!(!r.passed);
        assert!(r.max_gap > MAX_BASELINE_GAP);
    }

    #[test]
    fn an_op_missing_from_one_run_is_a_reproducibility_failure() {
        let runs = vec![
            run(100.0, 10.0),
            vec![baseline(Operation::ToolInvoke, 100.0)],
        ];
        let r = evaluate(1_000_000, 1_010_000, &runs);
        assert!(!r.passed);
        assert!(r.max_gap.is_infinite());
    }

    #[test]
    fn a_single_run_proves_nothing() {
        let r = evaluate(1_000_000, 1_010_000, &[run(100.0, 10.0)]);
        assert!(!r.passed); // nothing was reproduced
    }
}
