//! The **probe contract** — how substrate cost samples enter the ledger.
//!
//! Mechanism/policy split at L0: the ledger does not care where samples come
//! from. In CI they come from committed capture files ([`ReplayProbe`] — the
//! E6 harness replays them deterministically); on the bare-metal leg they come
//! from the live eBPF probe (CO-RE tracepoints/kprobes feeding a ring buffer),
//! which lands behind the `ebpf` feature when the box is up (RFC-0008 §9).

use std::collections::VecDeque;
use std::io::{self, BufRead, Write};

use crate::ledger::SubstrateEvent;

/// A source of substrate cost samples.
pub trait Probe {
    /// Drain whatever samples are ready.
    fn poll(&mut self) -> Vec<SubstrateEvent>;
    /// Stable identifier for telemetry.
    fn name(&self) -> &str;
}

/// Replays a committed capture — one JSON [`SubstrateEvent`] per line. This is
/// the CI-side probe: gate harnesses run against captures recorded on real
/// hardware, so CI verifies the *logic* without pretending to own a kernel.
pub struct ReplayProbe {
    events: VecDeque<SubstrateEvent>,
    /// How many events to release per poll (0 = all at once).
    batch: usize,
}

impl ReplayProbe {
    pub fn from_reader(reader: impl BufRead) -> io::Result<Self> {
        let mut events = VecDeque::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let ev: SubstrateEvent = serde_json::from_str(&line)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            events.push_back(ev);
        }
        Ok(Self { events, batch: 0 })
    }

    pub fn from_capture(capture: &str) -> io::Result<Self> {
        Self::from_reader(io::BufReader::new(capture.as_bytes()))
    }

    /// Release at most `n` events per [`poll`](Probe::poll) — exercises the
    /// incremental path the live probe will use.
    pub fn batched(mut self, n: usize) -> Self {
        self.batch = n;
        self
    }

    pub fn remaining(&self) -> usize {
        self.events.len()
    }
}

impl Probe for ReplayProbe {
    fn poll(&mut self) -> Vec<SubstrateEvent> {
        let n = if self.batch == 0 {
            self.events.len()
        } else {
            self.batch.min(self.events.len())
        };
        self.events.drain(..n).collect()
    }

    fn name(&self) -> &str {
        "replay"
    }
}

/// Write events as a capture (one JSON object per line) — the format the live
/// probe records and [`ReplayProbe`] replays.
pub fn write_capture(events: &[SubstrateEvent], mut w: impl Write) -> io::Result<()> {
    for ev in events {
        serde_json::to_writer(&mut w, ev)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(w)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::SubstrateCost;
    use thaliox_core::{AgentId, Operation};

    fn event(at: u64) -> SubstrateEvent {
        SubstrateEvent {
            agent: AgentId::new("a1"),
            op: Operation::VSend,
            at,
            cost: SubstrateCost {
                syscalls: 2,
                kernel_crossings: 4,
                ctx_switches: 0,
                on_cpu_ns: 500,
                bytes_copied: 4096,
            },
        }
    }

    #[test]
    fn capture_round_trips_through_replay() {
        let events = vec![event(1), event(2), event(3)];
        let mut buf = Vec::new();
        write_capture(&events, &mut buf).unwrap();

        let mut probe = ReplayProbe::from_reader(io::BufReader::new(buf.as_slice())).unwrap();
        let replayed = probe.poll();
        assert_eq!(replayed.len(), 3);
        assert_eq!(replayed[2].at, 3);
        assert_eq!(replayed[0].cost.bytes_copied, 4096);
        assert!(probe.poll().is_empty()); // drained
    }

    #[test]
    fn batched_replay_releases_incrementally() {
        let events = vec![event(1), event(2), event(3)];
        let mut buf = Vec::new();
        write_capture(&events, &mut buf).unwrap();

        let mut probe = ReplayProbe::from_capture(std::str::from_utf8(&buf).unwrap())
            .unwrap()
            .batched(2);
        assert_eq!(probe.poll().len(), 2);
        assert_eq!(probe.remaining(), 1);
        assert_eq!(probe.poll().len(), 1);
    }

    #[test]
    fn malformed_capture_is_rejected_not_skipped() {
        assert!(ReplayProbe::from_capture("{\"not\": \"an event\"}\n").is_err());
    }
}
