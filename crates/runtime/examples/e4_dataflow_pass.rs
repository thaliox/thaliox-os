//! E4 — Dataflow-scheduled Forward Pass falsification run (RFC-0003 §5).
//!
//! Checks that a forward-pass DAG is location-independent (every partition gives
//! the single-node result) while multi-node partitions overlap work — MELD
//! pillar 5.
//!
//! Usage:
//!   cargo run -p thaliox-runtime --example e4_dataflow_pass [seed]

use thaliox_runtime::experiment::run_e4;

fn main() {
    let seed = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(42);

    let report = run_e4(seed);

    println!("E4 — Dataflow-scheduled Forward Pass  (RFC-0003 §5)");
    println!(
        "seed = {}   serial makespan = {} steps\n",
        report.seed, report.serial_makespan
    );

    println!(
        "{:<20} {:>6} {:>9} {:>6} {:>6} {:>8} {:>9}",
        "partition", "nodes", "makespan", "conc", "msgs", "correct", "parallel"
    );
    println!("{}", "-".repeat(72));
    for s in &report.schedules {
        println!(
            "{:<20} {:>6} {:>9} {:>6} {:>6} {:>8} {:>9}",
            s.name,
            s.workers,
            s.makespan,
            s.max_concurrency,
            s.cross_node_msgs,
            if s.correct { "yes" } else { "NO" },
            if s.parallel { "yes" } else { "-" },
        );
    }

    println!();
    if report.dataflow_viable() {
        let speedup = report
            .schedules
            .iter()
            .filter(|s| s.parallel)
            .map(|s| s.makespan)
            .min()
            .unwrap_or(report.serial_makespan);
        println!("VERDICT: PASS — pillar 5 survives E4.");
        println!(
            "         Every placement yields the same result; multi-node cuts makespan {} → {}.",
            report.serial_makespan, speedup
        );
        println!("         Placement trades only cross-node messages — the OS scheduling lever.");
    } else {
        println!("VERDICT: KILL — a partition diverged or no partition overlapped work.");
        println!("         Redesign pillar 5 (RFC-0003 §5 failure criterion).");
    }
}
