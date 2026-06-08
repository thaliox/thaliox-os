//! E2 — Energy-based Latent Readout falsification run (RFC-0003 §5).
//!
//! Checks whether iterative energy minimization gives a monotone, saturating
//! steps↔quality curve — i.e. a real **AttentionBudget knob** for MELD pillar 3.
//!
//! Usage:
//!   cargo run -p thaliox-cognition --example e2_energy_readout [seed]

use thaliox_cognition::experiment::run_e2;

fn flag(ok: bool) -> &'static str {
    if ok { "yes" } else { "NO" }
}

fn main() {
    let seed = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(42);

    let report = run_e2(seed);

    println!("E2 — Energy-based Latent Readout  (RFC-0003 §5)");
    println!("seed = {}\n", report.seed);

    println!(
        "{:>6}  {:>12}  {:>10}  quality (lower = better)",
        "budget", "energy", "rmse"
    );
    println!("{}", "-".repeat(64));
    let worst = report.first().rmse.max(1e-6);
    for p in &report.curve {
        let filled = ((1.0 - (p.rmse / worst)) * 30.0).round().max(0.0) as usize;
        let bar: String = "#".repeat(filled);
        println!(
            "{:>6}  {:>12.5}  {:>10.5}  {}",
            p.steps, p.energy, p.rmse, bar
        );
    }

    println!(
        "\nenergy monotone: {}   improved: {}   saturating: {}",
        flag(report.energy_monotone),
        flag(report.improved),
        flag(report.saturating)
    );
    if report.knob_viable() {
        println!(
            "VERDICT: PASS — pillar 3 survives E2. Steps are a real, boundable AttentionBudget knob:"
        );
        println!(
            "         rmse {:.4} → {:.4} over {} steps, then plateaus.",
            report.first().rmse,
            report.last().rmse,
            report.last().steps
        );
    } else {
        println!("VERDICT: KILL — the steps↔quality curve is not a usable knob. Redesign pillar 3");
        println!("         (RFC-0003 §5 failure criterion).");
    }
}
