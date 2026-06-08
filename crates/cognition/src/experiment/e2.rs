//! # E2 — Energy-based Latent Readout (RFC-0003 §5)
//!
//! Falsification gate for **MELD pillar 3 — Energy-based Latent Readout**
//! (RFC-0003 §4): model "thinking" as iteratively lowering an energy in latent
//! space (diffusion-style denoising, parallel / non-autoregressive), with
//! **steps = compute**. If that holds, the step count *is* the
//! **AttentionBudget knob** (RFC-0001 §4; RFC-0002 §3.3) — the scheduler trades
//! compute for quality and INV-1 charges the spend.
//!
//! The decisive, toy-scale question:
//!
//! > Does iterative energy minimization yield a **monotone, saturating**
//! > steps↔quality curve — quality that reliably improves with more steps and
//! > then plateaus — so that "more thinking" is a real, *boundable* knob?
//!
//! E2 is deliberately **not** a neural net. We isolate the *readout dynamics*:
//! recover a hidden latent `z*` from noisy linear evidence `y = M z* + ε` by
//! descending the convex energy `E(z) = ‖M z − y‖² + λ‖z‖²`, starting from pure
//! noise. Every gradient step refines the **whole** latent at once
//! (non-autoregressive). We sweep the step count and test the curve against the
//! gate: energy must never rise, error must fall far, and the last budget
//! doubling must add little — the signature of a usable compute knob.
//!
//! Run it: `cargo run -p thaliox-cognition --example e2_energy_readout`.

/// Latent dimensionality (the quantity being read out).
pub const D: usize = 8;
/// Number of linear measurements. `K > D` ⇒ a well-conditioned, well-posed
/// inference, so descent converges fast enough to *saturate* within the sweep.
pub const K: usize = 64;

/// Deterministic xorshift32 — E2 must be reproducible (no `rand` dependency).
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

/// `y = M z`  (M is `K×D`, row-major).
fn matvec(m: &[f32], z: &[f32; D]) -> [f32; K] {
    let mut y = [0.0; K];
    for (k, yk) in y.iter_mut().enumerate() {
        let row = &m[k * D..k * D + D];
        *yk = row.iter().zip(z).map(|(a, b)| a * b).sum();
    }
    y
}

/// `r = Mᵀ v`  (`v` in `R^K`).
fn mat_t_vec(m: &[f32], v: &[f32; K]) -> [f32; D] {
    let mut r = [0.0; D];
    for (k, &vk) in v.iter().enumerate() {
        for (j, rj) in r.iter_mut().enumerate() {
            *rj += m[k * D + j] * vk;
        }
    }
    r
}

/// Convex energy `E(z) = ‖M z − y‖² + λ‖z‖²`. Its minimum is the data-consistent
/// readout; "thinking" descends it.
fn energy(m: &[f32], y: &[f32; K], z: &[f32; D], lambda: f32) -> f32 {
    let mz = matvec(m, z);
    let data: f32 = mz.iter().zip(y).map(|(a, b)| (a - b) * (a - b)).sum();
    let reg: f32 = z.iter().map(|v| v * v).sum();
    data + lambda * reg
}

/// `∇E = 2 Mᵀ(M z − y) + 2λ z`.
fn grad(m: &[f32], y: &[f32; K], z: &[f32; D], lambda: f32) -> [f32; D] {
    let mut resid = matvec(m, z);
    for (r, yy) in resid.iter_mut().zip(y) {
        *r -= *yy;
    }
    let mut g = mat_t_vec(m, &resid);
    for (gj, zj) in g.iter_mut().zip(z) {
        *gj = 2.0 * *gj + 2.0 * lambda * *zj;
    }
    g
}

fn rmse(a: &[f32; D], b: &[f32; D]) -> f32 {
    let s: f32 = a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum();
    (s / D as f32).sqrt()
}

/// One point on the steps↔quality curve.
#[derive(Debug, Clone, Copy)]
pub struct StepPoint {
    /// Refinement steps spent — the AttentionBudget for this readout.
    pub steps: usize,
    pub energy: f32,
    /// Error of the readout against the hidden `z*`.
    pub rmse: f32,
}

/// Full E2 report — the falsification verdict for pillar 3 (the budget knob).
#[derive(Debug, Clone)]
pub struct E2Report {
    pub seed: u32,
    pub curve: Vec<StepPoint>,
    /// Energy never rose across the *full* descent (thinking lowers energy).
    pub energy_monotone: bool,
    /// Final error is a small fraction of the starting error (the knob works).
    pub improved: bool,
    /// The last budget doubling yields a tiny fraction of the total gain (there
    /// is a sensible "enough" point — the knob is boundable, not bottomless).
    pub saturating: bool,
}

impl E2Report {
    /// Gate (RFC-0003 §5): the readout is a usable AttentionBudget knob iff the
    /// steps↔quality curve monotonically improves *and* saturates.
    /// `false` ⇒ kill / redesign pillar 3.
    pub fn knob_viable(&self) -> bool {
        self.energy_monotone && self.improved && self.saturating
    }
    pub fn first(&self) -> StepPoint {
        self.curve[0]
    }
    pub fn last(&self) -> StepPoint {
        *self.curve.last().unwrap()
    }
}

/// Run E2 deterministically for `seed`.
pub fn run_e2(seed: u32) -> E2Report {
    const LAMBDA: f32 = 0.05; // ridge — small: system is well-conditioned
    const ETA: f32 = 0.4; // step size (well within the stability limit)
    const MEAS_NOISE: f32 = 0.02;
    let sweep = [0usize, 1, 2, 4, 8, 16, 32, 64, 128];

    let mut rng = Rng::new(seed);

    // Hidden latent we must read out.
    let mut zstar = [0.0f32; D];
    for v in zstar.iter_mut() {
        *v = rng.signed();
    }

    // Measurement matrix M, scaled so each column has ~unit energy (stable GD),
    // and the noisy evidence y = M z* + ε.
    let scale = (3.0 / K as f32).sqrt();
    let mut m = vec![0.0f32; K * D];
    for e in m.iter_mut() {
        *e = rng.signed() * scale;
    }
    let mut y = matvec(&m, &zstar);
    for yk in y.iter_mut() {
        *yk += rng.signed() * MEAS_NOISE;
    }

    // Diffusion-style start: pure noise, refined toward the energy minimum.
    let mut z = [0.0f32; D];
    for v in z.iter_mut() {
        *v = rng.signed();
    }

    let max = *sweep.last().unwrap();
    let mut energies = Vec::with_capacity(max + 1);
    let mut curve = Vec::new();

    energies.push(energy(&m, &y, &z, LAMBDA));
    if sweep.contains(&0) {
        curve.push(StepPoint {
            steps: 0,
            energy: energies[0],
            rmse: rmse(&z, &zstar),
        });
    }
    for s in 1..=max {
        let g = grad(&m, &y, &z, LAMBDA);
        for (zj, gj) in z.iter_mut().zip(&g) {
            *zj -= ETA * *gj;
        }
        let e = energy(&m, &y, &z, LAMBDA);
        energies.push(e);
        if sweep.contains(&s) {
            curve.push(StepPoint {
                steps: s,
                energy: e,
                rmse: rmse(&z, &zstar),
            });
        }
    }

    let energy_monotone = energies.windows(2).all(|w| w[1] <= w[0] + 1e-4);
    let first = curve[0].rmse;
    let last = curve.last().unwrap().rmse;
    let improved = last < 0.25 * first;
    let total = first - last;
    let n = curve.len();
    let last_marginal = curve[n - 2].rmse - curve[n - 1].rmse;
    let saturating = total > 0.0 && last_marginal < 0.05 * total;

    E2Report {
        seed,
        curve,
        energy_monotone,
        improved,
        saturating,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn energy_decreases_monotonically() {
        assert!(run_e2(42).energy_monotone, "thinking must lower energy");
    }

    #[test]
    fn quality_improves_with_steps() {
        let r = run_e2(42);
        assert!(
            r.improved,
            "more steps should help: first {:.4} -> last {:.4}",
            r.first().rmse,
            r.last().rmse
        );
    }

    #[test]
    fn quality_saturates() {
        assert!(
            run_e2(42).saturating,
            "the knob must have an 'enough' point"
        );
    }

    #[test]
    fn knob_gate_passes() {
        assert!(run_e2(42).knob_viable());
    }

    #[test]
    fn curve_is_nonincreasing_in_rmse() {
        let r = run_e2(42);
        for w in r.curve.windows(2) {
            assert!(
                w[1].rmse <= w[0].rmse + 1e-3,
                "rmse rose: {:?} -> {:?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn deterministic_across_runs() {
        assert_eq!(
            run_e2(7).last().rmse.to_bits(),
            run_e2(7).last().rmse.to_bits()
        );
    }
}
