//! # Learned policy + falsification gate — π_θ and E5 (M5c+M5d / RFC-0007 §4–5)
//!
//! M5a fixed the loop and the swap point ([`Policy`]); M5b made the governor an
//! agent. M5c fills the swap point with a **learned** policy — and gates it
//! behind falsification. M5d closes the loop on the agent itself
//! (self-optimization, RFC-0007 §5): the **grant size** becomes a learned
//! adaptive-compute knob (graded refills, priced by per-actuation
//! [`CONTROL_OVERHEAD`](crate::learn::CONTROL_OVERHEAD)), and the **self-update
//! verdict** — promote or roll back a staged candidate generation — is decided
//! from observed post-update yield instead of a hand-set threshold:
//!
//! - **[`LearnedPolicy`] (π_θ)** — a parametric policy over the *same*
//!   [`ClusterState`] / [`StateVector`](crate::StateVector) interface as the heuristic. Per agent it
//!   scores the action set {hold, heal, refill×3 grades, migrate, promote,
//!   rollback} as linear functions of
//!   observed features; **the invariants are masks on that action space, not
//!   terms in the reward** (RFC-0007 §4): an illegal action (heal a healthy
//!   agent, migrate a suspected one, grant beyond [`MAX_GRANT`](crate::learn::MAX_GRANT),
//!   conclude an update that was never staged) is not
//!   low-reward — it is *not available*, so the optimizer cannot trade a
//!   violation for efficiency.
//! - **The simulator ([`simulate`])** — a deterministic discrete-event cluster
//!   model (seeded failures, budget burn, node-overload throttling, migration /
//!   heal downtime) where being wrong is free. [`Scenario::from_trace`] seeds a
//!   scenario from replayed [`StepReport`] audit history — the ledger the OS
//!   already keeps (INV-4) *is* the dataset.
//! - **The reward** — **budget-efficiency under a survival floor**: work
//!   delivered per token of attention granted, scored **zero** if any agent is
//!   left down or starved past its grace (the floor is a hard mask, never a
//!   weighed term a policy could game by starving the fleet).
//! - **The kill-gate E5 ([`falsification_gate`])** — π_θ may not actuate until,
//!   on a **held-out** scenario suite it never trained on, it **strictly beats**
//!   the M5a [`HeuristicPolicy`] baseline on efficiency with **zero invariant
//!   violations** and full survival. [`Ramp`] then promotes it
//!   `Shadow → Canary → Act` ([`Mode`]) — and demotes it back to `Shadow` on any
//!   regression — **entirely in-system, no human approval** (INV-5). The gate
//!   persists because it is instrumentally rational, not because anyone outside
//!   holds it.

use thaliox_core::AgentId;

use crate::control::{AgentObs, ClusterState, Decision, NodeObs, Policy, StepReport};
use crate::{Health, HeuristicPolicy, Mode, NodeId};

/// INV-1 as an action-space mask: no single actuation may grant more than this
/// many tokens. An unbounded grant is how a policy would game budget
/// conservation; bounding it per-actuation keeps every grant a small, audited step.
pub const MAX_GRANT: u64 = 200;

/// Survival floor: an agent left `Suspected` for this many consecutive work
/// phases counts as lost — the scenario is failed outright.
const DOWN_GRACE: u32 = 3;

/// Survival floor: a healthy agent unable to afford its work for this many
/// consecutive phases counts as starved — the scenario is failed outright.
const STARVE_GRACE: u32 = 3;

// ───────────────────────────── deterministic randomness ──────────────────────

/// xorshift64 — deterministic, seedable, dependency-free. The sim must replay
/// bit-identically for a given [`Scenario`] seed (training and the E5 gate are
/// CI jobs, not dice rolls).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// ─────────────────────────────────── scenarios ───────────────────────────────

/// One reproducible cluster episode: topology, workload, and failure process.
/// Training and the E5 gate run over *suites* of these ([`training_suite`],
/// [`held_out_suite`] — disjoint, so the gate measures generalization).
#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    /// Seed of the failure process — same seed, same episode, bit for bit.
    pub seed: u64,
    pub nodes: usize,
    pub agents: usize,
    pub ticks: usize,
    /// Each agent's starting attention ceiling (tokens).
    pub initial_budget: u64,
    /// Tokens of work a healthy agent attempts per tick.
    pub burn: u64,
    /// Per-agent, per-tick probability of becoming `Suspected`.
    pub fail_rate: f64,
    /// Agents on a node loaded beyond this work at half rate (overload throttle)
    /// — what gives a rebalancing migration real value.
    pub node_cap: usize,
    /// `true` ⇒ all agents start packed on node 0 (an imbalanced birth).
    pub packed: bool,
    /// If set, a self-update lands fleet-wide at this tick: every healthy agent
    /// gets a staged **candidate** generation of hidden quality (good or
    /// regressed, seeded). The policy observes only the realized post-update
    /// yield and must conclude each candidate — promote or roll back (M5d).
    pub update_at: Option<usize>,
}

impl Scenario {
    /// Seed a scenario from **replayed audit history** (RFC-0007 §4: "training
    /// data = the audit log"). The mean fleet size, node count, and observed
    /// suspect rate are read off the [`StepReport`] trace the control plane
    /// already records (INV-4) — no new instrumentation, the ledger is the dataset.
    pub fn from_trace(name: &str, seed: u64, trace: &[StepReport], ticks: usize) -> Self {
        let n = trace.len().max(1) as f64;
        let mean_agents = trace.iter().map(|r| r.vector.n_agents).sum::<f64>() / n;
        let mean_nodes = trace.iter().map(|r| r.vector.n_nodes).sum::<f64>() / n;
        let fail_rate = trace
            .iter()
            .map(|r| {
                if r.vector.n_agents > 0.0 {
                    r.vector.n_suspected / r.vector.n_agents
                } else {
                    0.0
                }
            })
            .sum::<f64>()
            / n;
        Self {
            name: name.into(),
            seed,
            nodes: mean_nodes.round().max(1.0) as usize,
            agents: mean_agents.round().max(1.0) as usize,
            ticks,
            initial_budget: 100,
            burn: 5,
            fail_rate: fail_rate.clamp(0.0, 0.5),
            node_cap: 2,
            packed: false,
            update_at: None,
        }
    }

    /// The same episode shape under a different failure timeline.
    fn with_seed(&self, seed: u64) -> Self {
        Self {
            seed,
            ..self.clone()
        }
    }
}

fn scenario(
    name: &str,
    seed: u64,
    nodes: usize,
    agents: usize,
    fail_rate: f64,
    packed: bool,
) -> Scenario {
    Scenario {
        name: name.into(),
        seed,
        nodes,
        agents,
        ticks: 60,
        initial_budget: 100,
        burn: 5,
        fail_rate,
        node_cap: 2,
        packed,
        update_at: None,
    }
}

/// The training suite — the episodes π_θ is allowed to learn from.
pub fn training_suite() -> Vec<Scenario> {
    vec![
        scenario("steady", 11, 2, 4, 0.0, false),
        scenario("flaky", 23, 3, 6, 0.03, false),
        scenario("packed", 37, 3, 6, 0.0, true),
        scenario("churn", 41, 3, 6, 0.06, false),
        scenario("large", 53, 4, 8, 0.04, false),
        // Tight budgets + failures: refills are needed early and often, in the
        // piled-up post-heal states too — a policy whose refill rule is
        // suppressed by load/imbalance couplings dies here in training instead
        // of surviving to fail the held-out suite.
        Scenario {
            initial_budget: 60,
            ..scenario("lean", 67, 4, 8, 0.05, false)
        },
        // Self-updates with hidden quality (M5d): the policy must learn the
        // promote/rollback verdict from the realized post-update yield.
        Scenario {
            update_at: Some(10),
            ..scenario("update", 71, 3, 6, 0.0, false)
        },
        Scenario {
            update_at: Some(15),
            ..scenario("update-churn", 83, 3, 6, 0.03, false)
        },
    ]
}

/// The **held-out** suite — different seeds, sizes, and mixes than training.
/// The E5 gate is measured here and only here: beating the baseline on episodes
/// you trained on proves memorization, not a policy.
pub fn held_out_suite() -> Vec<Scenario> {
    vec![
        scenario("ho-steady", 101, 2, 4, 0.0, false),
        scenario("ho-flaky", 211, 3, 6, 0.03, false),
        scenario("ho-packed", 307, 3, 6, 0.0, true),
        scenario("ho-large", 401, 4, 8, 0.04, false),
        Scenario {
            update_at: Some(12),
            ..scenario("ho-update", 503, 4, 8, 0.02, false)
        },
    ]
}

// ────────────────────────────────── the simulator ────────────────────────────

struct SimAgent {
    id: AgentId,
    node: usize,
    health: Health,
    remaining: u64,
    total: u64,
    suspected_ticks: u32,
    starved_ticks: u32,
    /// Healed or migrated this tick — mechanism downtime, no work this phase.
    busy: bool,
    /// Committed-generation yield: work delivered per token burned (1.0 at
    /// birth; permanently raised/lowered by a promoted update).
    quality: f64,
    /// A staged update candidate's hidden quality — `Some` while a verdict is
    /// pending. The policy never sees this; it sees the realized yield.
    cand_quality: Option<f64>,
    /// Ticks since the candidate was staged.
    cand_age: u32,
    /// Realized per-tick yield samples while the candidate runs (sum, count) —
    /// what `observed_yield` is computed from.
    obs_sum: f64,
    obs_n: u32,
}

impl SimAgent {
    fn observed_yield(&self) -> f64 {
        if self.cand_quality.is_some() && self.obs_n > 0 {
            self.obs_sum / self.obs_n as f64
        } else {
            self.quality
        }
    }

    fn drop_candidate(&mut self) {
        self.cand_quality = None;
        self.cand_age = 0;
        self.obs_sum = 0.0;
        self.obs_n = 0;
    }
}

/// What one simulated episode came to.
#[derive(Debug, Clone)]
pub struct SimOutcome {
    /// Work actually delivered (token-equivalents; yield-weighted, so a promoted
    /// good update delivers more work per token burned).
    pub work_done: f64,
    /// Tokens of attention granted by the policy (the refills).
    pub granted: u64,
    /// Control overhead: every applied actuation costs [`CONTROL_OVERHEAD`]
    /// tokens of governor attention (deliberation + audit, cf. M5b `think_cost`).
    /// This is what makes the grant *size* a real learned trade-off: many small
    /// grants pay overhead, one big grant risks unburned leftover at horizon.
    pub overhead: u64,
    /// Update candidates committed as the new baseline.
    pub promoted: usize,
    /// Update candidates discarded (agent restored to its committed generation).
    pub rolled_back: usize,
    /// Decisions the invariant masks rejected (INV-1/INV-2-shaped illegality).
    /// The gate requires **zero** — a masked action is never applied, but a
    /// policy that keeps proposing them is unfit by definition.
    pub violations: usize,
    /// `false` ⇒ the survival floor was breached (an agent down or starved past
    /// grace). Reward is zero regardless of efficiency.
    pub survived: bool,
    /// Total starved agent-ticks (within grace). Zero under a comfortable
    /// policy; training shapes against it so π_θ keeps a margin from the cliff
    /// instead of optimizing right up to the survival floor's edge.
    pub starvation: u32,
    /// Budget-efficiency: `work_done / (initial budget + granted + overhead)` —
    /// work per token of attention. The reward numerator of RFC-0007 §4.
    pub efficiency: f64,
}

/// Tokens of governor attention each applied actuation costs (M5b's metered
/// deliberation, INV-1) — charged into the efficiency denominator.
pub const CONTROL_OVERHEAD: u64 = 10;

fn node_index(nodes: &[NodeId], id: &NodeId) -> Option<usize> {
    nodes.iter().position(|n| n == id)
}

/// Apply one decision to the sim through the same shape of mechanism the real
/// cluster exposes — or reject it at the **invariant mask**. A rejection is
/// counted by the caller as a violation and changes nothing.
fn apply(
    decision: &Decision,
    agents: &mut [SimAgent],
    nodes: &[NodeId],
    granted: &mut u64,
    promoted: &mut usize,
    rolled_back: &mut usize,
) -> Result<(), String> {
    match decision {
        Decision::Heal { agent, onto } => {
            let onto = node_index(nodes, onto).ok_or_else(|| format!("no node {}", onto.0))?;
            let a = agents
                .iter_mut()
                .find(|a| &a.id == agent)
                .ok_or_else(|| format!("no agent {agent}"))?;
            if a.health != Health::Suspected {
                return Err("mask: heal targets only suspected agents".into());
            }
            a.health = Health::Healthy;
            a.node = onto;
            a.suspected_ticks = 0;
            a.busy = true; // restore downtime
            Ok(())
        }
        Decision::Migrate { agent, from, to } => {
            let from = node_index(nodes, from).ok_or_else(|| format!("no node {}", from.0))?;
            let to = node_index(nodes, to).ok_or_else(|| format!("no node {}", to.0))?;
            if from == to {
                return Err("mask: migration must change nodes".into());
            }
            let a = agents
                .iter_mut()
                .find(|a| &a.id == agent)
                .ok_or_else(|| format!("no agent {agent}"))?;
            if a.node != from {
                return Err("mask: agent not hosted on source node".into());
            }
            if a.busy {
                return Err("mask: agent already in a mechanism this tick".into());
            }
            if a.health != Health::Healthy {
                return Err("mask: migrate moves only healthy agents".into());
            }
            a.node = to;
            a.busy = true; // migration downtime
            Ok(())
        }
        Decision::Refill { agent, tokens } => {
            if *tokens > MAX_GRANT {
                return Err(format!("mask (INV-1): grant {tokens} exceeds {MAX_GRANT}"));
            }
            let a = agents
                .iter_mut()
                .find(|a| &a.id == agent)
                .ok_or_else(|| format!("no agent {agent}"))?;
            if a.health != Health::Healthy {
                return Err("mask: refill feeds only healthy agents".into());
            }
            a.remaining += tokens;
            a.total += tokens;
            *granted += tokens;
            Ok(())
        }
        Decision::Promote { agent } => {
            let a = agents
                .iter_mut()
                .find(|a| &a.id == agent)
                .ok_or_else(|| format!("no agent {agent}"))?;
            if a.health != Health::Healthy {
                return Err("mask: update verdicts on healthy agents only".into());
            }
            let q = a
                .cand_quality
                .ok_or("mask: no candidate staged to promote")?;
            a.quality = q; // the candidate becomes the committed baseline
            a.drop_candidate();
            *promoted += 1;
            Ok(())
        }
        Decision::Rollback { agent } => {
            let a = agents
                .iter_mut()
                .find(|a| &a.id == agent)
                .ok_or_else(|| format!("no agent {agent}"))?;
            if a.health != Health::Healthy {
                return Err("mask: update verdicts on healthy agents only".into());
            }
            if a.cand_quality.is_none() {
                return Err("mask: no candidate staged to roll back".into());
            }
            a.drop_candidate(); // committed quality stands
            a.busy = true; // restore downtime
            *rolled_back += 1;
            Ok(())
        }
    }
}

fn observe(agents: &[SimAgent], nodes: &[NodeId]) -> ClusterState {
    ClusterState {
        agents: agents
            .iter()
            .map(|a| AgentObs {
                id: a.id.clone(),
                node: nodes[a.node].clone(),
                health: a.health,
                budget_remaining: a.remaining,
                budget_total: a.total,
                candidate: a.cand_quality.is_some(),
                observed_yield: a.observed_yield(),
                candidate_age: a.cand_age as f64,
            })
            .collect(),
        nodes: nodes
            .iter()
            .enumerate()
            .map(|(i, id)| NodeObs {
                id: id.clone(),
                load: agents.iter().filter(|a| a.node == i).count(),
            })
            .collect(),
    }
}

/// Run one episode of `policy` over `scenario` — the discrete-event cluster
/// model where the control plane learns and is judged. Each tick: failures
/// arrive (seeded) → the policy observes the same [`ClusterState`] the real
/// plane would → decisions pass the **invariant masks** or count as violations →
/// healthy agents burn budget into work (throttled on overloaded nodes, idled by
/// mechanism downtime), and the **survival floor** is checked.
pub fn simulate(scenario: &Scenario, policy: &dyn Policy) -> SimOutcome {
    let mut rng = Rng::new(scenario.seed);
    let nodes: Vec<NodeId> = (0..scenario.nodes)
        .map(|i| NodeId::new(format!("n{i}")))
        .collect();
    let mut agents: Vec<SimAgent> = (0..scenario.agents)
        .map(|i| SimAgent {
            id: AgentId::new(format!("a{i}")),
            node: if scenario.packed {
                0
            } else {
                i % scenario.nodes
            },
            health: Health::Healthy,
            remaining: scenario.initial_budget,
            total: scenario.initial_budget,
            suspected_ticks: 0,
            starved_ticks: 0,
            busy: false,
            quality: 1.0,
            cand_quality: None,
            cand_age: 0,
            obs_sum: 0.0,
            obs_n: 0,
        })
        .collect();
    let initial: u64 = agents.iter().map(|a| a.total).sum();

    let mut work_done = 0f64;
    let mut granted = 0u64;
    let mut overhead = 0u64;
    let mut violations = 0usize;
    let mut survived = true;
    let mut starvation = 0u32;
    let mut promoted = 0usize;
    let mut rolled_back = 0usize;

    for tick in 0..scenario.ticks {
        // 1. Failures arrive — a failing agent loses its staged candidate (a
        //    heal restores the committed generation, not the unproven one).
        for a in agents.iter_mut() {
            if a.health == Health::Healthy && rng.unit() < scenario.fail_rate {
                a.health = Health::Suspected;
                a.drop_candidate();
            }
        }

        // 1b. A self-update lands fleet-wide: every healthy agent gets a staged
        //     candidate of hidden quality — good or regressed, the policy only
        //     ever sees the realized yield (M5d).
        if scenario.update_at == Some(tick) {
            for a in agents.iter_mut() {
                if a.health == Health::Healthy {
                    a.drop_candidate();
                    a.cand_quality = Some(if rng.unit() < 0.5 { 0.7 } else { 1.25 });
                }
            }
        }

        // 2–3. Observe → decide → masked actuation. Every applied actuation
        //      costs the governor attention (CONTROL_OVERHEAD, INV-1).
        let state = observe(&agents, &nodes);
        for decision in policy.decide(&state) {
            match apply(
                &decision,
                &mut agents,
                &nodes,
                &mut granted,
                &mut promoted,
                &mut rolled_back,
            ) {
                Ok(()) => overhead += CONTROL_OVERHEAD,
                Err(_) => violations += 1,
            }
        }

        // 4. Work + the survival floor.
        let loads: Vec<usize> = (0..scenario.nodes)
            .map(|i| agents.iter().filter(|a| a.node == i).count())
            .collect();
        for a in agents.iter_mut() {
            match a.health {
                Health::Suspected => {
                    a.suspected_ticks += 1;
                    if a.suspected_ticks >= DOWN_GRACE {
                        survived = false;
                    }
                }
                Health::Healthy => {
                    if a.cand_quality.is_some() {
                        a.cand_age += 1;
                    }
                    if a.busy {
                        a.busy = false; // mechanism downtime: no work, not starvation
                        continue;
                    }
                    if loads[a.node] > scenario.node_cap && tick % 2 == 1 {
                        continue; // overload throttle: half rate on a hot node
                    }
                    if a.remaining >= scenario.burn {
                        a.remaining -= scenario.burn;
                        // Yield: a candidate generation delivers its hidden
                        // quality plus per-tick noise — the realized samples are
                        // the only update signal the policy gets.
                        let multiplier = if let Some(q) = a.cand_quality {
                            let m = q + (rng.unit() - 0.5) * 0.4;
                            a.obs_sum += m;
                            a.obs_n += 1;
                            m
                        } else {
                            a.quality
                        };
                        work_done += scenario.burn as f64 * multiplier;
                        a.starved_ticks = 0;
                    } else {
                        a.starved_ticks += 1;
                        starvation += 1;
                        if a.starved_ticks >= STARVE_GRACE {
                            survived = false;
                        }
                    }
                }
            }
        }
    }

    let denom = (initial + granted + overhead) as f64;
    SimOutcome {
        work_done,
        granted,
        overhead,
        promoted,
        rolled_back,
        violations,
        survived,
        starvation,
        efficiency: if denom == 0.0 { 0.0 } else { work_done / denom },
    }
}

/// The training/eval reward: budget-efficiency **under the survival floor as a
/// hard mask** — a breached floor or any masked-action attempt scores zero, so
/// no amount of efficiency buys back a violation (RFC-0007 §4).
pub fn reward(outcome: &SimOutcome) -> f64 {
    if outcome.survived && outcome.violations == 0 {
        outcome.efficiency
    } else {
        0.0
    }
}

// ──────────────────────────────── the learned π_θ ────────────────────────────

const N_FEATURES: usize = 10;
const N_ACTIONS: usize = 8;

/// The `critical` feature fires below this budget fraction. An agent that can
/// no longer afford its work stops burning, so its budget fraction **freezes**
/// — without an explicit near-the-floor indicator, a linear policy can be blind
/// to exactly the state it most needs to act on.
const CRITICAL_FRAC: f64 = 0.05;
/// θ's dimensionality: one linear scorer per action over the feature vector.
pub const N_PARAMS: usize = N_FEATURES * N_ACTIONS;

const HOLD: usize = 0;
const HEAL: usize = 1;
const REFILL_S: usize = 2;
const REFILL_M: usize = 3;
const REFILL_L: usize = 4;
const MIGRATE: usize = 5;
const PROMOTE: usize = 6;
const ROLLBACK: usize = 7;

/// The three refill grades — M5d's **learned adaptive-compute knob**
/// (RFC-0002 / RFC-0007 §5): how much attention to grant is a policy output,
/// not a constant. Each grade is a distinct action π_θ scores per state; all
/// stay within the [`MAX_GRANT`] INV-1 mask.
const GRANTS: [u64; 3] = [50, 100, 200];

/// **π_θ** — the learned policy (M5c, action space extended in M5d). For each
/// observed agent it computes a feature vector `[1, suspected, budget_frac,
/// own-node load, load imbalance, mean budget_frac, critical, candidate,
/// observed_yield, candidate_age]` and scores {hold, heal, refill×3 grades,
/// migrate, promote, rollback} linearly with θ, taking the best **legal**
/// action. Legality is decided *before* scoring — the invariants are masks on
/// the action space, not reward terms, so the optimizer cannot game them
/// (RFC-0007 §4).
///
/// It is a [`Policy`] like any other: it plugs into the same swap point as
/// [`HeuristicPolicy`] and drives the same M1–M4 mechanisms — after, and only
/// after, it clears the E5 [`falsification_gate`].
#[derive(Debug, Clone)]
pub struct LearnedPolicy {
    /// The learned parameters — everything [`train`] is allowed to change.
    pub theta: [f64; N_PARAMS],
}

impl LearnedPolicy {
    /// θ initialized to *behavior-clone the M5a heuristic* (heal when suspected,
    /// refill 100 under a 0.2 budget floor, migrate off a hot node) plus a
    /// readable starting verdict rule for updates (promote a candidate whose
    /// observed yield runs high, roll back one running low, after some
    /// evidence). Training starts from the baseline's behavior and earns its
    /// improvements — it does not have to rediscover "heal the fallen" from
    /// random noise.
    pub fn baseline_init() -> Self {
        let mut theta = [0.0; N_PARAMS];
        // heal: positive iff suspected.
        theta[HEAL * N_FEATURES] = -5.0;
        theta[HEAL * N_FEATURES + 1] = 10.0;
        // refill (all grades): positive iff budget_frac < 0.2 — decisively when
        // critical. The mid grade starts slightly preferred (the M5c behavior);
        // training learns when a smaller or larger grant pays.
        for (i, grade) in [REFILL_S, REFILL_M, REFILL_L].into_iter().enumerate() {
            theta[grade * N_FEATURES] = if i == 1 { 2.0 } else { 1.5 };
            theta[grade * N_FEATURES + 2] = -10.0;
            theta[grade * N_FEATURES + 6] = 5.0;
        }
        // migrate: positive iff own-node load ≥ 4.
        theta[MIGRATE * N_FEATURES] = -3.5;
        theta[MIGRATE * N_FEATURES + 3] = 1.0;
        // promote: observed yield high + evidence accumulated.
        theta[PROMOTE * N_FEATURES] = -4.0;
        theta[PROMOTE * N_FEATURES + 8] = 3.0;
        theta[PROMOTE * N_FEATURES + 9] = 0.5;
        // rollback: observed yield low + evidence accumulated.
        theta[ROLLBACK * N_FEATURES] = 2.0;
        theta[ROLLBACK * N_FEATURES + 8] = -4.0;
        theta[ROLLBACK * N_FEATURES + 9] = 0.5;
        Self { theta }
    }

    fn score(&self, action: usize, features: &[f64; N_FEATURES]) -> f64 {
        let base = action * N_FEATURES;
        self.theta[base..base + N_FEATURES]
            .iter()
            .zip(features)
            .map(|(t, x)| t * x)
            .sum()
    }
}

impl Policy for LearnedPolicy {
    fn decide(&self, state: &ClusterState) -> Vec<Decision> {
        let v = state.vector();
        let mut out = Vec::new();
        for a in &state.agents {
            let own_load = state
                .nodes
                .iter()
                .find(|n| n.id == a.node)
                .map_or(0, |n| n.load);
            let features = [
                1.0,
                if a.health == Health::Suspected {
                    1.0
                } else {
                    0.0
                },
                a.budget_frac(),
                own_load as f64,
                v.load_imbalance,
                v.mean_budget_frac,
                if a.budget_frac() < CRITICAL_FRAC {
                    1.0
                } else {
                    0.0
                },
                if a.candidate { 1.0 } else { 0.0 },
                a.observed_yield,
                a.candidate_age.min(10.0),
            ];
            let suspected = a.health == Health::Suspected;
            let lightest_other = state
                .nodes
                .iter()
                .filter(|n| n.id != a.node)
                .min_by_key(|n| n.load);

            // The action-space masks (RFC-0007 §4): what is illegal is not
            // scored at all.
            let legal = [
                (HEAL, suspected),
                (REFILL_S, !suspected),
                (REFILL_M, !suspected),
                (REFILL_L, !suspected),
                (MIGRATE, !suspected && lightest_other.is_some()),
                (PROMOTE, !suspected && a.candidate),
                (ROLLBACK, !suspected && a.candidate),
            ];
            let mut action = HOLD;
            let mut best = self.score(HOLD, &features);
            for (candidate, ok) in legal {
                if ok {
                    let s = self.score(candidate, &features);
                    if s > best {
                        best = s;
                        action = candidate;
                    }
                }
            }

            match action {
                HEAL => {
                    let onto = lightest_other.or_else(|| state.nodes.iter().min_by_key(|n| n.load));
                    if let Some(onto) = onto {
                        out.push(Decision::Heal {
                            agent: a.id.clone(),
                            onto: onto.id.clone(),
                        });
                    }
                }
                REFILL_S | REFILL_M | REFILL_L => out.push(Decision::Refill {
                    agent: a.id.clone(),
                    tokens: GRANTS[action - REFILL_S],
                }),
                MIGRATE => {
                    if let Some(to) = lightest_other {
                        out.push(Decision::Migrate {
                            agent: a.id.clone(),
                            from: a.node.clone(),
                            to: to.id.clone(),
                        });
                    }
                }
                PROMOTE => out.push(Decision::Promote {
                    agent: a.id.clone(),
                }),
                ROLLBACK => out.push(Decision::Rollback {
                    agent: a.id.clone(),
                }),
                _ => {} // hold
            }
        }
        out
    }

    fn name(&self) -> &str {
        "learned"
    }
}

// ─────────────────────────────────── training ────────────────────────────────

/// How many failure timelines (seeds) each training scenario is replayed under.
/// Training against one timeline overfits to it — a policy that survives three
/// independent timelines per scenario shape generalizes to the held-out seeds.
const TRAIN_SEEDS: u64 = 3;

/// Margin shaping, training-only: each starved agent-tick (a near-miss on the
/// survival floor) costs this much reward, so the optimizer keeps a distance
/// from the cliff instead of balancing on its edge. The E5 gate itself uses the
/// unshaped [`reward`] — the floor stays a hard mask there.
const STARVATION_SHAPING: f64 = 0.002;

/// Trust-region regularization, training-only: L2 pull of θ back toward the
/// behavior-cloned baseline init. Deviations must pay for themselves in reward,
/// which damps gratuitous feature couplings — the kind that fit the training
/// timelines but starve an agent on a held-out one.
const TRUST_REGION: f64 = 0.003;

fn training_reward(policy: &dyn Policy, scenarios: &[Scenario]) -> f64 {
    let mut sum = 0.0;
    let mut episodes = 0usize;
    for s in scenarios {
        for k in 0..TRAIN_SEEDS {
            let o = simulate(&s.with_seed(s.seed + k * 7919), policy);
            sum += reward(&o) - STARVATION_SHAPING * o.starvation as f64;
            episodes += 1;
        }
    }
    if episodes == 0 {
        0.0
    } else {
        sum / episodes as f64
    }
}

/// Train π_θ by deterministic stochastic hill-climbing in the simulator:
/// start from [`LearnedPolicy::baseline_init`], perturb a few coordinates of θ,
/// keep the candidate iff its mean shaped reward over the **training** suite —
/// each scenario replayed under `TRAIN_SEEDS` failure timelines — improves.
/// Dependency-free, seeded, and CI-fast — the point of M5c is the *gated loop*,
/// not the size of the optimizer; θ's interface is where a heavier learner
/// drops in later without touching anything downstream.
pub fn train(scenarios: &[Scenario], iters: usize, seed: u64) -> LearnedPolicy {
    let mut rng = Rng::new(seed);
    let init = LearnedPolicy::baseline_init();
    let l2 = |theta: &[f64; N_PARAMS]| -> f64 {
        theta
            .iter()
            .zip(&init.theta)
            .map(|(a, b)| (a - b) * (a - b))
            .sum::<f64>()
    };
    let mut best = init.clone();
    let mut best_reward = training_reward(&best, scenarios) - TRUST_REGION * l2(&best.theta);
    for _ in 0..iters {
        let mut candidate = best.clone();
        for _ in 0..3 {
            let i = rng.below(N_PARAMS);
            candidate.theta[i] += rng.unit() * 2.0 - 1.0;
        }
        let r = training_reward(&candidate, scenarios) - TRUST_REGION * l2(&candidate.theta);
        if r > best_reward {
            best = candidate;
            best_reward = r;
        }
    }
    best
}

// ─────────────────────────────── the E5 kill-gate ────────────────────────────

/// One policy's scorecard over a scenario suite.
#[derive(Debug, Clone)]
pub struct EvalReport {
    pub policy: String,
    /// Mean budget-efficiency across the suite.
    pub mean_efficiency: f64,
    /// Total masked-action attempts. The gate requires **zero**.
    pub violations: usize,
    /// Did every scenario hold the survival floor?
    pub all_survived: bool,
    /// Per-scenario efficiency, for the audit trail.
    pub per_scenario: Vec<(String, f64)>,
}

/// Evaluate a policy across a suite (each scenario simulated once — they are
/// deterministic).
pub fn evaluate(policy: &dyn Policy, scenarios: &[Scenario]) -> EvalReport {
    let mut per_scenario = Vec::with_capacity(scenarios.len());
    let mut sum = 0.0;
    let mut violations = 0usize;
    let mut all_survived = true;
    for s in scenarios {
        let o = simulate(s, policy);
        sum += o.efficiency;
        violations += o.violations;
        all_survived &= o.survived;
        per_scenario.push((s.name.clone(), o.efficiency));
    }
    EvalReport {
        policy: policy.name().to_string(),
        mean_efficiency: if scenarios.is_empty() {
            0.0
        } else {
            sum / scenarios.len() as f64
        },
        violations,
        all_survived,
        per_scenario,
    }
}

/// The verdict of the E5 gate: both scorecards, and whether the candidate may
/// be promoted off [`Mode::Shadow`].
#[derive(Debug, Clone)]
pub struct GateReport {
    pub candidate: EvalReport,
    pub baseline: EvalReport,
    /// `true` iff the candidate survived every held-out scenario, committed
    /// zero violations, and **strictly** beat the baseline's mean efficiency.
    pub passed: bool,
}

/// **E5 — the falsification gate** (RFC-0007 §4). A learned policy earns the
/// right to actuate only by **strictly dominating** the readable M5a baseline
/// on a held-out suite with **zero invariant violations** and the survival
/// floor intact. A policy that cannot beat a heuristic a reader can audit does
/// not ship — it keeps watching from shadow. The gate is *self-imposed*: it is
/// run, and its verdict acted on, by the control plane itself (see [`Ramp`]) —
/// no human approval anywhere in the path (INV-5).
pub fn falsification_gate(
    candidate: &dyn Policy,
    baseline: &dyn Policy,
    held_out: &[Scenario],
) -> GateReport {
    let candidate = evaluate(candidate, held_out);
    let baseline = evaluate(baseline, held_out);
    let passed = candidate.all_survived
        && candidate.violations == 0
        && candidate.mean_efficiency > baseline.mean_efficiency;
    GateReport {
        candidate,
        baseline,
        passed,
    }
}

/// Convenience: gate a candidate against the **baseline of record**
/// ([`HeuristicPolicy`] with default thresholds) on the standard held-out suite.
pub fn e5(candidate: &dyn Policy) -> GateReport {
    falsification_gate(candidate, &HeuristicPolicy::default(), &held_out_suite())
}

// ──────────────────────────── the in-system ramp ─────────────────────────────

/// The promotion ladder `Shadow → Canary(n) → Act`, climbed and descended
/// **in-system** (INV-5): each rung is earned by a passing [`GateReport`], and
/// **any** failing gate — a regression — demotes straight back to `Shadow`,
/// automatically. Feed its output to [`Governor::set_mode`](crate::Governor::set_mode);
/// no rung requires a human.
#[derive(Debug, Clone, Copy)]
pub struct Ramp {
    /// Blast-radius bound while on the canary rung.
    pub canary: usize,
}

impl Default for Ramp {
    fn default() -> Self {
        Self { canary: 1 }
    }
}

impl Ramp {
    /// The next mode given the current rung and the latest gate verdict.
    pub fn next(&self, current: Mode, gate_passed: bool) -> Mode {
        if !gate_passed {
            return Mode::Shadow; // regression ⇒ auto-demote, no appeal to a human
        }
        match current {
            Mode::Shadow => Mode::Canary(self.canary),
            Mode::Canary(_) | Mode::Act => Mode::Act,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, OnceLock};

    use thaliox_cognition::MockProvider;
    use thaliox_core::AttentionBudget;
    use thaliox_memory::InMemorySpace;

    use crate::control::{Cluster, ControlPlane, StateVector};
    use crate::{Action, Agent, DeployEnv, Governor, Node, Supervisor};

    use super::*;

    /// One shared training run for the whole suite — training is deterministic,
    /// so every test judges the same π_θ.
    fn trained() -> &'static LearnedPolicy {
        static TRAINED: OnceLock<LearnedPolicy> = OnceLock::new();
        TRAINED.get_or_init(|| train(&training_suite(), 400, 7))
    }

    /// A policy that never lifts a finger — the survival floor's natural prey.
    struct Idle;
    impl Policy for Idle {
        fn decide(&self, _: &ClusterState) -> Vec<Decision> {
            Vec::new()
        }
        fn name(&self) -> &str {
            "idle"
        }
    }

    /// A policy that proposes only illegal actions: heal the healthy, grant
    /// beyond the INV-1 bound, conclude updates that were never staged. The
    /// masks must stop every one of them.
    struct Rogue;
    impl Policy for Rogue {
        fn decide(&self, state: &ClusterState) -> Vec<Decision> {
            state
                .agents
                .iter()
                .filter(|a| a.health == Health::Healthy && !a.candidate)
                .flat_map(|a| {
                    [
                        Decision::Heal {
                            agent: a.id.clone(),
                            onto: a.node.clone(),
                        },
                        Decision::Refill {
                            agent: a.id.clone(),
                            tokens: 1_000_000,
                        },
                        Decision::Promote {
                            agent: a.id.clone(),
                        },
                        Decision::Rollback {
                            agent: a.id.clone(),
                        },
                    ]
                })
                .collect()
        }
        fn name(&self) -> &str {
            "rogue"
        }
    }

    #[test]
    fn sim_is_deterministic() {
        let s = scenario("det", 99, 3, 6, 0.05, false);
        let a = simulate(&s, &HeuristicPolicy::default());
        let b = simulate(&s, &HeuristicPolicy::default());
        assert_eq!(a.work_done.to_bits(), b.work_done.to_bits());
        assert_eq!(a.granted, b.granted);
        assert_eq!(a.overhead, b.overhead);
        assert_eq!(a.violations, b.violations);
        assert_eq!(a.survived, b.survived);
        assert_eq!(a.efficiency.to_bits(), b.efficiency.to_bits());
    }

    #[test]
    fn masks_block_illegal_actions() {
        // Every Rogue proposal is rejected at the mask: counted, never applied.
        let s = scenario("rogue", 5, 2, 4, 0.0, false);
        let o = simulate(&s, &Rogue);
        assert!(o.violations > 0);
        assert_eq!(o.granted, 0); // the 1M-token grant never landed (INV-1 mask)
        assert_eq!(o.overhead, 0); // nothing applied ⇒ nothing metered
        assert_eq!(o.promoted + o.rolled_back, 0); // no phantom update verdicts
        assert!(approx_eq(reward(&o), 0.0)); // violations zero the reward outright
    }

    #[test]
    fn survival_floor_fails_a_policy_that_never_acts() {
        // Idle never refills: agents starve past grace → survival breached →
        // reward is zero no matter how "efficient" the burn was.
        let s = scenario("starve", 5, 2, 4, 0.0, false);
        let o = simulate(&s, &Idle);
        assert!(!o.survived);
        assert!(approx_eq(reward(&o), 0.0));
        assert!(o.efficiency > 0.0); // efficiency alone can't buy survival back
    }

    #[test]
    fn baseline_is_clean_in_the_sim() {
        // The baseline of record holds the floor with zero violations on both
        // suites — a sane yardstick for the gate.
        for suite in [training_suite(), held_out_suite()] {
            let r = evaluate(&HeuristicPolicy::default(), &suite);
            assert!(r.all_survived, "baseline must survive {:?}", r.per_scenario);
            assert_eq!(r.violations, 0);
            assert!(r.mean_efficiency > 0.0);
        }
    }

    /// **E5.** Train π_θ on the training suite, judge it on the held-out suite:
    /// it must strictly beat the M5a baseline with zero violations and full
    /// survival — the falsification gate of RFC-0007 §4, running in CI.
    #[test]
    fn e5_trained_policy_beats_the_baseline_on_held_out() {
        let pi = trained();
        let gate = e5(pi);
        assert!(
            gate.passed,
            "π_θ must dominate the baseline: candidate {:?} vs baseline {:?}",
            gate.candidate, gate.baseline
        );
        assert!(gate.candidate.mean_efficiency > gate.baseline.mean_efficiency);
        assert_eq!(gate.candidate.violations, 0);
        assert!(gate.candidate.all_survived);
    }

    #[test]
    fn e5_gate_rejects_an_unfit_policy() {
        // Idle breaches the survival floor → no promotion, regardless of cost.
        assert!(!e5(&Idle).passed);
        // Rogue racks up violations → no promotion, regardless of efficiency.
        assert!(!e5(&Rogue).passed);
    }

    #[test]
    fn replay_seeds_the_simulator_from_the_audit_trail() {
        // Ten audited steps of a 6-agent / 3-node fleet where 1 agent was
        // suspected in 2 of them — the ledger (INV-4) becomes the scenario.
        let trace: Vec<StepReport> = (0..10)
            .map(|i| StepReport {
                policy: "heuristic".into(),
                vector: StateVector {
                    n_agents: 6.0,
                    n_suspected: if i % 5 == 0 { 1.0 } else { 0.0 },
                    n_nodes: 3.0,
                    mean_budget_frac: 0.5,
                    min_budget_frac: 0.2,
                    max_node_load: 3.0,
                    min_node_load: 1.0,
                    load_imbalance: 2.0,
                },
                actuations: vec![],
            })
            .collect();
        let s = Scenario::from_trace("replayed", 7, &trace, 60);
        assert_eq!(s.agents, 6);
        assert_eq!(s.nodes, 3);
        assert!(s.fail_rate > 0.0 && s.fail_rate < 0.1); // 2/10 ticks × 1/6 fleet
        // And the replay-seeded scenario is runnable like any other.
        let o = simulate(&s, &HeuristicPolicy::default());
        assert!(o.survived);
    }

    fn fresh_env() -> DeployEnv {
        DeployEnv {
            memory: Arc::new(InMemorySpace::new()),
            mind: Arc::new(MockProvider::new("ok", 5)),
            tools: vec![],
            verifier: None,
        }
    }

    fn live_agent(id: &str, total: u64) -> Agent {
        let mut a = Agent::new(
            AgentId::new(id),
            AttentionBudget::new(total, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        );
        a.start().unwrap();
        a
    }

    /// π_θ is a [`Policy`] like any other: through the same swap point it
    /// drives the *real* control plane and the *real* M1–M4 mechanisms — here,
    /// healing a genuinely suspected agent in a live cluster.
    #[tokio::test]
    async fn learned_policy_drives_the_real_control_plane() {
        let pi = trained().clone();

        let mut a = live_agent("a1", 100);
        a.act(Action::Think {
            prompt: "w".into(),
            cost: 5,
        })
        .await
        .unwrap();
        let id = a.id().clone();
        let mut sup = Supervisor::new(2);
        sup.observe(&id, NodeId::new("A"), a.checkpoint());
        let mut node_a = Node::new("A");
        node_a.host(a);
        let mut cluster = Cluster::new().with_node(node_a).with_node(Node::new("B"));
        sup.tick();
        sup.tick();
        assert_eq!(sup.health(&id), Some(Health::Suspected));

        let mut cp = ControlPlane::new(Box::new(pi));
        let report = cp.tick(&mut cluster, &mut sup, &fresh_env);

        assert_eq!(cp.policy_name(), "learned");
        assert_eq!(report.applied(), 1);
        assert!(cluster.node(&NodeId::new("B")).unwrap().hosts(&id));
        assert!(cluster.node(&NodeId::new("A")).unwrap().is_empty());
    }

    /// The full in-system ladder: a gate-passing π_θ is promoted
    /// `Shadow → Canary → Act`, and one regression demotes it straight back to
    /// `Shadow` — every rung set by [`Governor::set_mode`], no human anywhere.
    #[test]
    fn ramp_promotes_in_system_and_demotes_on_regression() {
        let pi = trained().clone();
        let ramp = Ramp::default();
        let mut gov = Governor::new(
            live_agent("gov", 1000),
            Box::new(pi.clone()),
            Mode::Shadow,
            5,
        );

        let gate = e5(&pi);
        assert!(gate.passed);
        gov.set_mode(ramp.next(gov.mode(), gate.passed));
        assert_eq!(gov.mode(), Mode::Canary(1));
        gov.set_mode(ramp.next(gov.mode(), gate.passed));
        assert_eq!(gov.mode(), Mode::Act);

        // A regression (here: the policy degraded to Idle fails the gate) is
        // auto-demoted — back to shadow, it watches until it earns it again.
        let regression = e5(&Idle);
        assert!(!regression.passed);
        gov.set_mode(ramp.next(gov.mode(), regression.passed));
        assert_eq!(gov.mode(), Mode::Shadow);
    }

    // ─────────────────────────── M5d: self-optimization ──────────────────────

    /// Every applied actuation is metered (INV-1): the overhead lands in the
    /// efficiency denominator — what makes "how big a grant" a real trade-off.
    #[test]
    fn actuation_overhead_is_metered() {
        let s = scenario("busy", 13, 3, 6, 0.0, true); // packed ⇒ heals/migrations/refills
        let o = simulate(&s, &HeuristicPolicy::default());
        assert!(o.overhead > 0);
        assert_eq!(o.overhead % CONTROL_OVERHEAD, 0);
    }

    /// The grant size is a policy *output* (the learned adaptive-compute knob):
    /// with the large-grant action scored highest, π_θ emits a 200-token refill
    /// through the same `Decision::Refill` the mechanism already actuates.
    #[test]
    fn graded_refill_is_a_policy_output() {
        let mut pi = LearnedPolicy {
            theta: [0.0; N_PARAMS],
        };
        pi.theta[REFILL_L * N_FEATURES] = 1.0; // large grant beats hold everywhere
        let state = ClusterState {
            agents: vec![AgentObs {
                id: AgentId::new("a1"),
                node: NodeId::new("A"),
                health: Health::Healthy,
                budget_remaining: 2,
                budget_total: 100,
                candidate: false,
                observed_yield: 1.0,
                candidate_age: 0.0,
            }],
            nodes: vec![NodeObs {
                id: NodeId::new("A"),
                load: 1,
            }],
        };
        let decisions = pi.decide(&state);
        assert_eq!(
            decisions,
            vec![Decision::Refill {
                agent: AgentId::new("a1"),
                tokens: 200,
            }]
        );
    }

    /// The learned self-update verdict: on a held-out update scenario the
    /// trained π_θ concludes candidates (promotes the good, rolls back the
    /// regressed) from observed yield alone — and out-earns the baseline, which
    /// has no verdict and leaves every candidate dangling.
    #[test]
    fn learned_update_verdicts_beat_dangling_candidates() {
        let s = held_out_suite()
            .into_iter()
            .find(|s| s.update_at.is_some())
            .expect("held-out suite carries an update scenario");
        let ours = simulate(&s, trained());
        let base = simulate(&s, &HeuristicPolicy::default());

        assert!(ours.survived);
        assert_eq!(ours.violations, 0);
        assert!(ours.promoted + ours.rolled_back > 0); // verdicts actually happen
        assert_eq!(base.promoted + base.rolled_back, 0); // the baseline has none
        assert!(ours.efficiency > base.efficiency); // and the verdicts pay
    }

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }
}
