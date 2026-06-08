//! # E4 — Dataflow-scheduled Forward Pass (RFC-0003 §5)
//!
//! Falsification gate for **MELD pillar 5 — Dataflow Execution** (RFC-0003 §4):
//! a forward "pass" is not a monolithic kernel but a **dependency graph** the OS
//! schedules across cores / microVMs / nodes. For that to be real, two things
//! must hold at once:
//!
//! - **Location-independence** — the result is *identical* no matter how the ops
//!   are partitioned across nodes. This is the property that lets the scheduler
//!   place, and later migrate (RFC-0001 §6), any op anywhere.
//! - **Realized parallelism** — partitioning across ≥2 nodes actually *overlaps*
//!   independent ops, so the makespan beats the serial run. If spreading the
//!   graph buys no concurrency, "dataflow" is a hollow label.
//!
//! The decisive, toy-scale question:
//!
//! > Do *all* partitions of a small forward-pass DAG compute the same result as
//! > a single-node run, and does *some* multi-node partition finish strictly
//! > faster with ≥2 ops running at once?
//!
//! E4 is deliberately **not** a real model. We isolate the *scheduling
//! semantics*: a width-2, depth-2 graph of small linear+ReLU ops with two
//! independent branches (so genuine parallelism exists). A list scheduler honors
//! dependencies, runs one ready op per node per step, and reports makespan,
//! peak concurrency, and **cross-node messages** (each dependency edge that
//! crosses a partition boundary — a TAM `vsend`/`vrecv`, RFC-0001 §3). Placement
//! changes the transport cost but never the result — the OS's scheduling lever.
//!
//! Run it: `cargo run -p thaliox-runtime --example e4_dataflow_pass`.

/// Vector width of every op's input/output.
pub const DIM: usize = 4;

/// Deterministic xorshift32 — E4 must be reproducible (no `rand` dependency).
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Rng(seed | 1)
    }
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// Uniform in `[-1, 1)`.
    fn signed(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    }
    fn matrix(&mut self) -> Vec<f32> {
        (0..DIM * DIM).map(|_| self.signed()).collect()
    }
    fn vector(&mut self) -> Vec<f32> {
        (0..DIM).map(|_| self.signed()).collect()
    }
}

/// What an op computes from its dependencies.
enum OpKind {
    /// A resident input (data already present at the node — finishes at t=0).
    Input(Vec<f32>),
    /// `relu(W · dep0)`.
    Linear(Vec<f32>),
    /// `Wa · dep0 + Wb · dep1` (the merge of two branches).
    AddProj(Vec<f32>, Vec<f32>),
}

/// A node in the forward-pass DAG.
pub struct OpNode {
    pub name: &'static str,
    pub deps: Vec<usize>,
    kind: OpKind,
}

fn matvec(w: &[f32], x: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0; DIM];
    for (r, o) in out.iter_mut().enumerate() {
        let row = &w[r * DIM..r * DIM + DIM];
        *o = row.iter().zip(x).map(|(a, b)| a * b).sum();
    }
    out
}

fn compute(kind: &OpKind, deps: &[&[f32]]) -> Vec<f32> {
    match kind {
        OpKind::Input(v) => v.clone(),
        OpKind::Linear(w) => matvec(w, deps[0]).into_iter().map(|x| x.max(0.0)).collect(),
        OpKind::AddProj(wa, wb) => {
            let a = matvec(wa, deps[0]);
            let b = matvec(wb, deps[1]);
            a.iter().zip(&b).map(|(x, y)| x + y).collect()
        }
    }
}

/// Build the toy forward-pass graph deterministically. Two independent branches
/// (1,3) and (2,4) feed a merge (5); the input (0) is resident.
///
/// ```text
///         ┌─ 1:lin ─ 3:lin ─┐
///  0:in ──┤                 ├─ 5:add ─▶ y
///         └─ 2:lin ─ 4:lin ─┘
/// ```
fn build_graph(seed: u32) -> Vec<OpNode> {
    let mut rng = Rng::new(seed);
    vec![
        OpNode {
            name: "x",
            deps: vec![],
            kind: OpKind::Input(rng.vector()),
        },
        OpNode {
            name: "a1",
            deps: vec![0],
            kind: OpKind::Linear(rng.matrix()),
        },
        OpNode {
            name: "a2",
            deps: vec![0],
            kind: OpKind::Linear(rng.matrix()),
        },
        OpNode {
            name: "b1",
            deps: vec![1],
            kind: OpKind::Linear(rng.matrix()),
        },
        OpNode {
            name: "b2",
            deps: vec![2],
            kind: OpKind::Linear(rng.matrix()),
        },
        OpNode {
            name: "y",
            deps: vec![3, 4],
            kind: OpKind::AddProj(rng.matrix(), rng.matrix()),
        },
    ]
}

/// Result of scheduling the graph under one partition.
struct SimResult {
    output: Vec<f32>,
    makespan: usize,
    max_concurrency: usize,
    cross_node_msgs: usize,
}

/// List-schedule `graph` under `partition` (op → node) across `workers` nodes.
/// One ready op per node per discrete step; inputs are resident at t=0.
fn simulate(graph: &[OpNode], partition: &[usize], workers: usize) -> SimResult {
    let n = graph.len();
    let mut value: Vec<Option<Vec<f32>>> = vec![None; n];
    let mut finish = vec![0usize; n];

    // Inputs are already resident — no step consumed.
    for (i, node) in graph.iter().enumerate() {
        if let OpKind::Input(v) = &node.kind {
            value[i] = Some(v.clone());
        }
    }
    let mut done = value.iter().filter(|v| v.is_some()).count();

    let mut time = 0;
    let mut max_concurrency = 0;
    while done < n {
        time += 1;
        let mut busy = vec![false; workers];
        let mut starting = Vec::new();
        for (i, node) in graph.iter().enumerate() {
            if value[i].is_some() {
                continue;
            }
            let ready = node.deps.iter().all(|&d| value[d].is_some());
            if ready && !busy[partition[i]] {
                busy[partition[i]] = true;
                starting.push(i);
            }
        }
        if starting.is_empty() {
            break; // deadlock guard — a well-formed DAG never hits this
        }
        max_concurrency = max_concurrency.max(starting.len());
        for &i in &starting {
            let v = {
                let deps: Vec<&[f32]> = graph[i]
                    .deps
                    .iter()
                    .map(|&d| value[d].as_ref().unwrap().as_slice())
                    .collect();
                compute(&graph[i].kind, &deps)
            };
            value[i] = Some(v);
            finish[i] = time;
            done += 1;
        }
    }

    let mut cross_node_msgs = 0;
    for (i, node) in graph.iter().enumerate() {
        for &d in &node.deps {
            if partition[i] != partition[d] {
                cross_node_msgs += 1;
            }
        }
    }

    SimResult {
        output: value[n - 1].clone().unwrap(),
        makespan: *finish.iter().max().unwrap(),
        max_concurrency,
        cross_node_msgs,
    }
}

/// One scheduled placement and how it fared.
#[derive(Debug, Clone)]
pub struct Schedule {
    pub name: &'static str,
    pub workers: usize,
    pub makespan: usize,
    pub max_concurrency: usize,
    pub cross_node_msgs: usize,
    /// Result is bit-identical to the single-node reference.
    pub correct: bool,
    /// Finishes strictly faster than serial with ≥2 ops overlapping.
    pub parallel: bool,
}

/// Full E4 report — the verdict for pillar 5.
#[derive(Debug, Clone)]
pub struct E4Report {
    pub seed: u32,
    pub serial_makespan: usize,
    pub schedules: Vec<Schedule>,
}

impl E4Report {
    /// Gate (RFC-0003 §5): dataflow execution holds iff **every** partition is
    /// location-independent (correct) and **some** multi-node partition realizes
    /// parallelism. `false` ⇒ kill / redesign pillar 5.
    pub fn dataflow_viable(&self) -> bool {
        self.schedules.iter().all(|s| s.correct) && self.schedules.iter().any(|s| s.parallel)
    }
}

fn bit_eq(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// Run E4 deterministically for `seed`.
pub fn run_e4(seed: u32) -> E4Report {
    let graph = build_graph(seed);

    // Reference: everything on one node.
    let reference = simulate(&graph, &[0, 0, 0, 0, 0, 0], 1);
    let serial_makespan = reference.makespan;

    // Partitions: op index → node. Same graph, different placement.
    let plans: [(&str, [usize; 6], usize); 4] = [
        ("monolith (1 node)", [0, 0, 0, 0, 0, 0], 1),
        ("balanced (2 nodes)", [0, 0, 1, 0, 1, 0], 2), // each branch on its own node
        ("fragmented (2 nodes)", [0, 0, 1, 1, 0, 1], 2), // within-branch edges cross — more messages
        ("three nodes", [0, 0, 1, 0, 1, 2], 3),
    ];

    let schedules = plans
        .iter()
        .map(|(name, part, workers)| {
            let r = simulate(&graph, part, *workers);
            Schedule {
                name,
                workers: *workers,
                makespan: r.makespan,
                max_concurrency: r.max_concurrency,
                cross_node_msgs: r.cross_node_msgs,
                correct: bit_eq(&r.output, &reference.output),
                parallel: r.makespan < serial_makespan && r.max_concurrency >= 2,
            }
        })
        .collect();

    E4Report {
        seed,
        serial_makespan,
        schedules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_partition_is_location_independent() {
        for s in run_e4(42).schedules {
            assert!(s.correct, "{} diverged from the single-node result", s.name);
        }
    }

    #[test]
    fn some_partition_realizes_parallelism() {
        assert!(run_e4(42).schedules.iter().any(|s| s.parallel));
    }

    #[test]
    fn multi_node_beats_serial() {
        let r = run_e4(42);
        let balanced = r
            .schedules
            .iter()
            .find(|s| s.name == "balanced (2 nodes)")
            .unwrap();
        assert!(balanced.makespan < r.serial_makespan);
        assert!(balanced.max_concurrency >= 2);
    }

    #[test]
    fn placement_changes_transport_not_result() {
        let r = run_e4(42);
        let balanced = r
            .schedules
            .iter()
            .find(|s| s.name.starts_with("balanced"))
            .unwrap();
        let fragmented = r
            .schedules
            .iter()
            .find(|s| s.name.starts_with("fragmented"))
            .unwrap();
        // Both correct, but the fragmented placement pays more cross-node messages.
        assert!(balanced.correct && fragmented.correct);
        assert!(fragmented.cross_node_msgs > balanced.cross_node_msgs);
    }

    #[test]
    fn gate_passes() {
        assert!(run_e4(42).dataflow_viable());
    }

    #[test]
    fn deterministic_across_runs() {
        let a = run_e4(7);
        let b = run_e4(7);
        assert_eq!(a.schedules[1].makespan, b.schedules[1].makespan);
        assert!(bit_eq(
            // results stable across runs
            &simulate(&build_graph(7), &[0, 0, 1, 0, 1, 0], 2).output,
            &simulate(&build_graph(7), &[0, 1, 0, 1, 0, 1], 2).output,
        ));
    }
}
