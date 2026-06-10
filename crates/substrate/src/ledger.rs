//! The **substrate ledger** — TAM operations priced in substrate cost.
//!
//! RFC-0008 §3a: probes emit `(agent, tam_op, substrate_cost)` samples; the
//! ledger joins them with the INV-4 audit stream, so every audited call gains a
//! substrate-cost column. Per-op [`Baseline`]s summarize the ledger — they are
//! the denominators of every later M6 gate, which is why E6 demands they be
//! cheap to collect and reproducible before anything else may proceed.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thaliox_core::{AgentId, AuditRecord, Operation};

/// What one TAM operation cost the substrate. All counters are deltas for the
/// attributed span, not absolutes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubstrateCost {
    /// Syscalls issued.
    pub syscalls: u64,
    /// User↔kernel boundary crossings (≥ syscalls; includes faults, signals).
    pub kernel_crossings: u64,
    /// Involuntary + voluntary context switches.
    pub ctx_switches: u64,
    /// On-CPU time, nanoseconds.
    pub on_cpu_ns: u64,
    /// Bytes copied through the kernel (read/write/send/recv paths).
    pub bytes_copied: u64,
}

impl SubstrateCost {
    fn accumulate(&mut self, other: &SubstrateCost) {
        self.syscalls += other.syscalls;
        self.kernel_crossings += other.kernel_crossings;
        self.ctx_switches += other.ctx_switches;
        self.on_cpu_ns += other.on_cpu_ns;
        self.bytes_copied += other.bytes_copied;
    }

    fn as_array(&self) -> [f64; 5] {
        [
            self.syscalls as f64,
            self.kernel_crossings as f64,
            self.ctx_switches as f64,
            self.on_cpu_ns as f64,
            self.bytes_copied as f64,
        ]
    }
}

/// One cost sample emitted by a [`Probe`](crate::Probe), attributed to an agent
/// and a TAM operation at a point in time (unix millis — the same clock domain
/// as [`AuditRecord::at`], which is what makes the join possible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubstrateEvent {
    pub agent: AgentId,
    pub op: Operation,
    pub at: u64,
    pub cost: SubstrateCost,
}

/// One audited call with its substrate-cost column filled in.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub audit: AuditRecord,
    pub cost: SubstrateCost,
    /// How many probe samples were folded into `cost`.
    pub samples: usize,
}

/// Per-operation cost summary — the **baseline** later M6 stages are measured
/// against (and E6's reproducibility subject).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub op: Operation,
    /// Calls observed.
    pub n: usize,
    /// Mean cost per call, per metric (same order as the cost fields).
    pub mean: [f64; 5],
}

impl Baseline {
    /// The largest relative gap between two baselines for the same op, across
    /// metrics — E6's reproducibility measure. Metrics near zero on both sides
    /// contribute no gap.
    pub fn relative_gap(&self, other: &Baseline) -> f64 {
        self.mean
            .iter()
            .zip(&other.mean)
            .map(|(a, b)| {
                let denom = a.abs().max(b.abs());
                if denom < f64::EPSILON {
                    0.0
                } else {
                    (a - b).abs() / denom
                }
            })
            .fold(0.0, f64::max)
    }
}

/// Joins probe samples to audited calls: the audit ledger (INV-4) gains its
/// substrate-cost column. Events that match no audited call within the window
/// are kept — counted, never silently dropped — as `unattributed`.
pub struct SubstrateLedger {
    window_ms: u64,
    entries: Vec<LedgerEntry>,
    unattributed: Vec<SubstrateEvent>,
}

impl SubstrateLedger {
    /// `window_ms`: how far (in time) a sample may sit from its audited call
    /// and still be attributed to it.
    pub fn new(window_ms: u64) -> Self {
        Self {
            window_ms,
            entries: Vec::new(),
            unattributed: Vec::new(),
        }
    }

    /// Join a batch of audit records with a batch of probe samples. Each sample
    /// is attributed to the nearest-in-time audit record of the **same agent
    /// and operation** within the window.
    pub fn join(&mut self, audits: &[AuditRecord], events: Vec<SubstrateEvent>) {
        let base = self.entries.len();
        self.entries.extend(audits.iter().map(|a| LedgerEntry {
            audit: a.clone(),
            cost: SubstrateCost::default(),
            samples: 0,
        }));
        for ev in events {
            let best = self.entries[base..]
                .iter_mut()
                .filter(|e| e.audit.agent == ev.agent && e.audit.op == ev.op)
                .map(|e| (e.audit.at.abs_diff(ev.at), e))
                .filter(|(gap, _)| *gap <= self.window_ms)
                .min_by_key(|(gap, _)| *gap);
            match best {
                Some((_, entry)) => {
                    entry.cost.accumulate(&ev.cost);
                    entry.samples += 1;
                }
                None => self.unattributed.push(ev),
            }
        }
    }

    /// All audited calls with their substrate-cost column.
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// Samples no audited call claimed — a high count means the probes see
    /// work the audit stream does not explain, which is itself a finding.
    pub fn unattributed(&self) -> &[SubstrateEvent] {
        &self.unattributed
    }

    /// Per-operation baselines over every attributed entry, ordered by op for
    /// deterministic output.
    pub fn baselines(&self) -> Vec<Baseline> {
        let mut acc: HashMap<Operation, (usize, [f64; 5])> = HashMap::new();
        for e in &self.entries {
            let (n, sum) = acc.entry(e.audit.op).or_insert((0, [0.0; 5]));
            *n += 1;
            for (s, v) in sum.iter_mut().zip(e.cost.as_array()) {
                *s += v;
            }
        }
        let mut out: Vec<Baseline> = acc
            .into_iter()
            .map(|(op, (n, sum))| Baseline {
                op,
                n,
                mean: sum.map(|s| s / n as f64),
            })
            .collect();
        out.sort_by_key(|b| format!("{:?}", b.op));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thaliox_core::Permission;

    fn audit(agent: &str, op: Operation, at: u64) -> AuditRecord {
        AuditRecord {
            agent: AgentId::new(agent),
            op,
            permission_used: Some(Permission::Execute),
            cost: 5,
            target: "t".into(),
            at,
            allowed: true,
        }
    }

    fn event(agent: &str, op: Operation, at: u64, syscalls: u64) -> SubstrateEvent {
        SubstrateEvent {
            agent: AgentId::new(agent),
            op,
            at,
            cost: SubstrateCost {
                syscalls,
                kernel_crossings: syscalls * 2,
                ctx_switches: 1,
                on_cpu_ns: 1_000,
                bytes_copied: 64,
            },
        }
    }

    #[test]
    fn join_attributes_samples_to_the_nearest_matching_call() {
        let mut ledger = SubstrateLedger::new(50);
        let audits = vec![
            audit("a1", Operation::ToolInvoke, 1_000),
            audit("a1", Operation::ToolInvoke, 1_200), // nearer to the 1_190 sample
        ];
        let events = vec![
            event("a1", Operation::ToolInvoke, 1_010, 3),
            event("a1", Operation::ToolInvoke, 1_190, 7),
        ];
        ledger.join(&audits, events);

        assert_eq!(ledger.entries().len(), 2);
        assert_eq!(ledger.entries()[0].cost.syscalls, 3);
        assert_eq!(ledger.entries()[1].cost.syscalls, 7);
        assert_eq!(ledger.entries()[1].samples, 1);
        assert!(ledger.unattributed().is_empty());
    }

    #[test]
    fn agent_and_op_must_match_for_attribution() {
        let mut ledger = SubstrateLedger::new(50);
        let audits = vec![audit("a1", Operation::ToolInvoke, 1_000)];
        let events = vec![
            event("a2", Operation::ToolInvoke, 1_000, 3), // wrong agent
            event("a1", Operation::MemRead, 1_000, 4),    // wrong op
            event("a1", Operation::ToolInvoke, 2_000, 5), // outside window
        ];
        ledger.join(&audits, events);

        assert_eq!(ledger.entries()[0].samples, 0);
        // Counted, never dropped: unexplained substrate work is a finding.
        assert_eq!(ledger.unattributed().len(), 3);
    }

    #[test]
    fn baselines_average_per_operation() {
        let mut ledger = SubstrateLedger::new(50);
        let audits = vec![
            audit("a1", Operation::ToolInvoke, 1_000),
            audit("a1", Operation::ToolInvoke, 2_000),
            audit("a1", Operation::Think, 3_000),
        ];
        let events = vec![
            event("a1", Operation::ToolInvoke, 1_000, 10),
            event("a1", Operation::ToolInvoke, 2_000, 20),
        ];
        ledger.join(&audits, events);
        let baselines = ledger.baselines();

        let tool = baselines
            .iter()
            .find(|b| b.op == Operation::ToolInvoke)
            .unwrap();
        assert_eq!(tool.n, 2);
        assert!((tool.mean[0] - 15.0).abs() < 1e-9); // (10+20)/2 syscalls
        let think = baselines.iter().find(|b| b.op == Operation::Think).unwrap();
        assert!((think.mean[0]).abs() < 1e-9); // no substrate samples → zero cost
    }

    #[test]
    fn relative_gap_measures_reproducibility() {
        let a = Baseline {
            op: Operation::ToolInvoke,
            n: 10,
            mean: [100.0, 200.0, 1.0, 1e6, 64.0],
        };
        let mut b = a.clone();
        assert!(a.relative_gap(&b) < 1e-12); // identical runs → zero gap
        b.mean[0] = 109.0; // 9% off on syscalls
        let gap = a.relative_gap(&b);
        assert!(gap > 0.08 && gap < 0.10);
    }
}
