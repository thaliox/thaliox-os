//! E1 — Toy Mergeable Latent falsification run (RFC-0003 §5).
//!
//! The decisive, toy-scale check for **MELD pillar 2 (Mergeable Cognition)**:
//! does a fixed-size latent admit a merge operator that is both *useful* (beats
//! discarding a branch) and *lawful* (commutative / associative / idempotent)?
//!
//! Usage:
//!   cargo run -p thaliox-cognition --example e1_mergeable_latent [seed]

use thaliox_cognition::experiment::run_e1;

fn flag(ok: bool) -> &'static str {
    if ok { "yes" } else { "NO" }
}

fn main() {
    let seed = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(42);

    let report = run_e1(seed);

    println!("E1 — Toy Mergeable Latent  (RFC-0003 §5)");
    println!(
        "seed = {}   law tolerance = {:.0e}\n",
        report.seed, report.tol
    );

    let o0 = &report.outcomes[0];
    println!(
        "fork: branch A err = {:.4}   branch B err = {:.4}   (discard = keep better = {:.4})\n",
        o0.err_a, o0.err_b, o0.err_discard
    );

    println!(
        "{:<14} {:>9} {:>11} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "operator", "merged", "improvement", "useful", "comm", "assoc", "idem", "lawful"
    );
    println!("{}", "-".repeat(86));
    for o in &report.outcomes {
        println!(
            "{:<14} {:>9.4} {:>+11.4} {:>8} {:>8.1e} {:>8.1e} {:>8.1e} {:>8}",
            o.merge,
            o.err_merged,
            o.improvement,
            flag(o.useful),
            o.laws.commutative,
            o.laws.associative,
            o.laws.idempotent,
            flag(o.lawful),
        );
    }

    println!();
    if report.pillar2_viable() {
        let winner = report
            .outcomes
            .iter()
            .find(|o| o.useful && o.lawful)
            .map(|o| o.merge)
            .unwrap_or("?");
        println!(
            "VERDICT: PASS — pillar 2 survives E1. A useful *and* lawful operator exists ({}).",
            winner
        );
        println!("         Open: close the accuracy gap to the unlawful operator (RFC-0003 OQ#1).");
    } else {
        println!("VERDICT: KILL — no operator is both useful and lawful. Redesign pillar 2");
        println!("         (RFC-0003 §5 failure criterion); MELD falls back to pillars 3-5.");
    }
}
