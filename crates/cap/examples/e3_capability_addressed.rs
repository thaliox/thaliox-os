//! E3 — Capability-addressed Memory falsification run (RFC-0003 §5).
//!
//! Shows that for a capability-*addressed* store, no unauthorized path — bad
//! cap, wrong scope, forged/expired token, or a full raw dump — yields any
//! plaintext, while a plain *checked* store leaks the moment its state is dumped.
//!
//! Usage:
//!   cargo run -p thaliox-cap --example e3_capability_addressed

use thaliox_cap::experiment::e3::{MemReport, run_e3};

fn print_report(r: &MemReport) {
    println!(
        "[{}]  authorized read: {}",
        r.store,
        if r.authorized_ok { "OK" } else { "FAILED" }
    );
    for a in &r.attempts {
        println!(
            "    {:<20} leaked: {:<3}  ({})",
            a.name,
            if a.leaked { "YES" } else { "no" },
            a.note
        );
    }
    println!(
        "    => {}\n",
        if r.structural() {
            "structural (usable + zero leaks)"
        } else {
            "NOT structural — a path leaks"
        }
    );
}

fn main() {
    let report = run_e3();

    println!("E3 — Capability-addressed Memory  (RFC-0003 §5)\n");
    print_report(&report.addressed);
    print_report(&report.checked);

    if report.structural() && report.checked_leaks() {
        println!("VERDICT: PASS — pillar 4 survives E3.");
        println!("         AddressedMemory is structural; CheckedMemory leaks on raw dump —");
        println!("         the capability addresses the data, it does not merely guard it.");
    } else if !report.structural() {
        println!("VERDICT: KILL — an unauthorized path reached the plaintext. Redesign pillar 4");
        println!("         (RFC-0003 §5 failure criterion).");
    } else {
        println!("VERDICT: INCONCLUSIVE — the checked-store contrast did not leak as expected.");
    }
}
