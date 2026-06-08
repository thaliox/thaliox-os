//! # E1 — Toy Mergeable Latent (RFC-0003 §5)
//!
//! Falsification gate for **MELD pillar 2 — Mergeable Cognition** (RFC-0003 §4),
//! which is itself THALIOX's answer to **RFC-0001 Open Question #3** ("is CRDT
//! merge sufficient for semantic state?"). The question E1 settles, at toy
//! scale, is narrow and decisive:
//!
//! > Does a *fixed-size latent state* admit a merge operator `⊕` that is at once
//! > **useful** — merging two diverged agents beats discarding either branch —
//! > and **lawful** — commutative, associative, idempotent (the CRDT laws)?
//!
//! E1 is deliberately **not** a neural network. Pillar 2 stands or falls on the
//! *merge primitive*, so we isolate it: a "latent" here is the fixed-size
//! evidence an agent has accumulated about a hidden ground-truth vector. We then
//! pit two candidate `⊕` operators against the RFC's success/failure criteria:
//!
//! - [`Merge::WeightedMean`] — accurate, but **breaks idempotency** (re-merging
//!   double-counts confidence), so it corrupts the repeated merges M3 needs
//!   (self-healing re-merges the same checkpoint).
//! - [`Merge::MaxConfidence`] — a true lattice join: commutative, associative,
//!   **and** idempotent, while still beating a single branch by covering the
//!   union of observations.
//!
//! **Gate rule** ([`E1Report::pillar2_viable`]): pillar 2 survives iff *some*
//! operator is both useful and lawful. The tension between the two operators is
//! the empirical grip on RFC-0003 Open Question #1 ("what latent geometry admits
//! a *useful* CRDT merge"), reported rather than hidden.
//!
//! Run it: `cargo run -p thaliox-cognition --example e1_mergeable_latent`.

/// Latent dimensionality of the toy state.
pub const D: usize = 32;

/// Deterministic xorshift32 — E1 must be reproducible (no `rand` dependency).
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
    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }
    /// Uniform in `[-1, 1)`.
    fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

/// A fixed-size latent: per-dimension best estimate and the confidence behind it.
/// `conf[d] == 0.0` means dimension `d` was never observed.
#[derive(Clone, PartialEq)]
pub struct Latent {
    pub value: [f32; D],
    pub conf: [f32; D],
}

impl Latent {
    pub fn empty() -> Self {
        Self {
            value: [0.0; D],
            conf: [0.0; D],
        }
    }

    /// An agent's *own* learning (intra-agent, not the merge under test):
    /// fold one observation of `dim` into a running, confidence-weighted mean.
    fn observe(&mut self, dim: usize, v: f32, c: f32) {
        let nc = self.conf[dim] + c;
        if nc > 0.0 {
            self.value[dim] = (self.value[dim] * self.conf[dim] + v * c) / nc;
            self.conf[dim] = nc;
        }
    }

    /// Task error: RMSE of the estimate against ground truth over *all* dims.
    /// Unobserved dims estimate `0.0`, so wider coverage ⇒ lower error — which is
    /// exactly why merging two partial agents should help.
    fn rmse(&self, target: &[f32; D]) -> f32 {
        let s: f32 = self
            .value
            .iter()
            .zip(target)
            .map(|(v, t)| (v - t) * (v - t))
            .sum();
        (s / D as f32).sqrt()
    }
}

/// Distance over the *full* latent (value **and** confidence). Confidence is part
/// of the state because it drives future merges — so the CRDT laws must hold over
/// it too. This is what exposes [`Merge::WeightedMean`]'s broken idempotency.
fn dist(a: &Latent, b: &Latent) -> f32 {
    let s: f32 = a
        .value
        .iter()
        .zip(&b.value)
        .zip(a.conf.iter().zip(&b.conf))
        .map(|((av, bv), (ac, bc))| (av - bv) * (av - bv) + (ac - bc) * (ac - bc))
        .sum();
    (s / D as f32).sqrt()
}

/// A candidate cross-agent merge operator `⊕` under falsification.
#[derive(Clone, Copy)]
pub enum Merge {
    /// Confidence-weighted mean. Accurate; commutative & associative; **not**
    /// idempotent (confidence sums, so `a ⊕ a ≠ a`).
    WeightedMean,
    /// Lattice join: per dim keep the higher-confidence estimate (deterministic
    /// tie-break). Commutative, associative, **and** idempotent.
    MaxConfidence,
}

impl Merge {
    pub fn name(self) -> &'static str {
        match self {
            Merge::WeightedMean => "WeightedMean",
            Merge::MaxConfidence => "MaxConfidence",
        }
    }

    /// Apply `a ⊕ b`.
    pub fn apply(self, a: &Latent, b: &Latent) -> Latent {
        let mut out = Latent::empty();
        for d in 0..D {
            match self {
                Merge::WeightedMean => {
                    let c = a.conf[d] + b.conf[d];
                    out.conf[d] = c;
                    out.value[d] = if c > 0.0 {
                        (a.value[d] * a.conf[d] + b.value[d] * b.conf[d]) / c
                    } else {
                        0.0
                    };
                }
                Merge::MaxConfidence => {
                    // Lexicographic argmax on (conf, value): order-independent, so
                    // the join is commutative and associative; equal states map to
                    // themselves, so it is idempotent.
                    let take_a = a.conf[d] > b.conf[d]
                        || (a.conf[d] == b.conf[d] && a.value[d] >= b.value[d]);
                    let (v, c) = if take_a {
                        (a.value[d], a.conf[d])
                    } else {
                        (b.value[d], b.conf[d])
                    };
                    out.value[d] = v;
                    out.conf[d] = c;
                }
            }
        }
        out
    }
}

/// Worst-case residuals of the three CRDT laws over random sampled states.
/// Zero (within tolerance) means the law holds.
#[derive(Debug, Clone, Copy)]
pub struct Laws {
    pub commutative: f32,
    pub associative: f32,
    pub idempotent: f32,
}

fn random_latent(rng: &mut Rng) -> Latent {
    let mut l = Latent::empty();
    for d in 0..D {
        if rng.unit() < 0.7 {
            l.value[d] = rng.signed();
            l.conf[d] = rng.unit() + 0.1;
        }
    }
    l
}

/// Measure the worst-case law residuals for `m` over `n` random triples.
fn measure_laws(m: Merge, rng: &mut Rng, n: usize) -> Laws {
    let mut comm = 0.0f32;
    let mut assoc = 0.0f32;
    let mut idem = 0.0f32;
    for _ in 0..n {
        let a = random_latent(rng);
        let b = random_latent(rng);
        let c = random_latent(rng);
        comm = comm.max(dist(&m.apply(&a, &b), &m.apply(&b, &a)));
        assoc = assoc.max(dist(
            &m.apply(&m.apply(&a, &b), &c),
            &m.apply(&a, &m.apply(&b, &c)),
        ));
        idem = idem.max(dist(&m.apply(&a, &a), &a));
    }
    Laws {
        commutative: comm,
        associative: assoc,
        idempotent: idem,
    }
}

fn make_target(rng: &mut Rng) -> [f32; D] {
    let mut t = [0.0; D];
    for v in t.iter_mut() {
        *v = rng.signed();
    }
    t
}

/// Train one branch on a partial, noisy view of `target` (coverage < 1.0 ⇒ the
/// branch only "knows" some dimensions — divergence is the point).
fn train_branch(target: &[f32; D], rng: &mut Rng, coverage: f32, obs_per_dim: usize) -> Latent {
    const NOISE: f32 = 0.15;
    let mut l = Latent::empty();
    for (d, &tv) in target.iter().enumerate() {
        if rng.unit() < coverage {
            for _ in 0..obs_per_dim {
                let v = tv + rng.signed() * NOISE;
                let c = 1.0 + rng.unit(); // varied confidence so the join is meaningful
                l.observe(d, v, c);
            }
        }
    }
    l
}

/// Outcome for one merge operator.
#[derive(Debug, Clone)]
pub struct StrategyOutcome {
    pub merge: &'static str,
    pub err_a: f32,
    pub err_b: f32,
    /// Error of the better single branch — i.e. of *discarding* the other.
    pub err_discard: f32,
    pub err_merged: f32,
    /// `err_discard - err_merged` (positive ⇒ merge helped).
    pub improvement: f32,
    pub laws: Laws,
    /// Strictly beats discarding either branch.
    pub useful: bool,
    /// All three CRDT-law residuals within tolerance.
    pub lawful: bool,
}

/// Full E1 report — the falsification verdict for pillar 2.
#[derive(Debug, Clone)]
pub struct E1Report {
    pub seed: u32,
    pub tol: f32,
    pub outcomes: Vec<StrategyOutcome>,
}

impl E1Report {
    /// The gate (RFC-0003 §5): pillar 2 survives iff *some* operator is both
    /// useful and lawful. `false` ⇒ kill / redesign pillar 2.
    pub fn pillar2_viable(&self) -> bool {
        self.outcomes.iter().any(|o| o.useful && o.lawful)
    }
}

/// Run E1 deterministically for `seed`.
pub fn run_e1(seed: u32) -> E1Report {
    let tol = 1e-3;
    let mut rng = Rng::new(seed);

    let target = make_target(&mut rng);
    // Fork: two branches diverge on different partial evidence.
    let a = train_branch(&target, &mut rng, 0.6, 3);
    let b = train_branch(&target, &mut rng, 0.6, 3);

    let err_a = a.rmse(&target);
    let err_b = b.rmse(&target);
    let err_discard = err_a.min(err_b);

    let mut outcomes = Vec::new();
    for m in [Merge::WeightedMean, Merge::MaxConfidence] {
        let merged = m.apply(&a, &b);
        let err_merged = merged.rmse(&target);
        let laws = measure_laws(m, &mut rng, 64);
        let useful = err_merged < err_discard - 1e-6;
        let lawful = laws.commutative < tol && laws.associative < tol && laws.idempotent < tol;
        outcomes.push(StrategyOutcome {
            merge: m.name(),
            err_a,
            err_b,
            err_discard,
            err_merged,
            improvement: err_discard - err_merged,
            laws,
            useful,
            lawful,
        });
    }

    E1Report {
        seed,
        tol,
        outcomes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_confidence_is_lawful() {
        let o = run_e1(42)
            .outcomes
            .into_iter()
            .find(|o| o.merge == "MaxConfidence")
            .unwrap();
        assert!(
            o.lawful,
            "MaxConfidence should satisfy CRDT laws: {:?}",
            o.laws
        );
    }

    #[test]
    fn weighted_mean_breaks_idempotency() {
        let r = run_e1(42);
        let o = r
            .outcomes
            .iter()
            .find(|o| o.merge == "WeightedMean")
            .unwrap();
        assert!(
            o.laws.idempotent > r.tol,
            "WeightedMean idempotency should fail (confidence double-counts): {:?}",
            o.laws
        );
    }

    #[test]
    fn merge_beats_discarding_a_branch() {
        for o in run_e1(42).outcomes {
            assert!(
                o.useful,
                "{} not useful: merged {:.4} vs discard {:.4}",
                o.merge, o.err_merged, o.err_discard
            );
        }
    }

    #[test]
    fn pillar2_gate_passes() {
        // A lawful AND useful operator exists ⇒ pillar 2 survives E1.
        assert!(run_e1(42).pillar2_viable());
    }

    #[test]
    fn deterministic_across_runs() {
        let x = run_e1(7);
        let y = run_e1(7);
        assert_eq!(
            x.outcomes[1].err_merged.to_bits(),
            y.outcomes[1].err_merged.to_bits()
        );
    }
}
