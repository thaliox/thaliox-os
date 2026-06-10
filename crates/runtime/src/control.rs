//! # Control plane — the L4 governor (M5a / RFC-0007)
//!
//! The closed loop where THALIOX governs itself: **observe** the cluster as a
//! fixed-width vector → run a **policy** → **actuate** *only* through the
//! mechanisms M1–M4 already shipped ([`Supervisor::self_heal`], runtime
//! [`migrate`](crate::migrate), [`Agent::grant_budget`]). The control plane
//! invents no new way to touch an agent; it only chooses *which* invariant-guarded
//! operation to invoke, and *when*.
//!
//! This is the strict mechanism/policy split (TAM §4.2): M1–M4 are the mechanism,
//! the [`Policy`] is the only swappable part. M5a ships a transparent
//! [`HeuristicPolicy`] — the **baseline of record** that M5c's learned `π_θ` must
//! out-perform before it is allowed to actuate. There is no human in the loop
//! (INV-5 self-sovereignty); the loop's discipline is self-imposed.
//!
//! Each [`tick`](ControlPlane::tick) yields a [`StepReport`] — the governor's own
//! audit trail (INV-4), and the ledger a future learned policy trains on.

use std::collections::HashMap;

use thaliox_core::{AgentId, AuditRecord, Permission, ResourceKind};

use crate::update::CheckpointHistory;
use crate::{Action, Agent, DeployEnv, Health, Node, NodeId, Supervisor};

/// A bag of [`Node`]s the control plane actuates over. It owns the nodes so it can
/// hand the right `&mut Node`(s) to the runtime mechanisms (`self_heal`,
/// `migrate`) without the caller juggling disjoint borrows. It also keeps each
/// agent's generational [`CheckpointHistory`] (M2 `update.rs`), so self-update
/// verdicts ([`Decision::Promote`] / [`Decision::Rollback`], M5d) actuate through
/// the same mechanism path as everything else.
#[derive(Default)]
pub struct Cluster {
    nodes: Vec<Node>,
    histories: HashMap<AgentId, CheckpointHistory>,
}

impl Cluster {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style: add a node and return self.
    pub fn with_node(mut self, node: Node) -> Self {
        self.nodes.push(node);
        self
    }

    pub fn add(&mut self, node: Node) {
        self.nodes.push(node);
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id() == id)
    }

    pub fn node_mut(&mut self, id: &NodeId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id() == id)
    }

    fn index_of(&self, id: &NodeId) -> Option<usize> {
        self.nodes.iter().position(|n| n.id() == id)
    }

    /// Migrate `agent` from one node to another, resolving the two disjoint
    /// `&mut Node` borrows internally before calling runtime [`migrate`](crate::migrate).
    pub fn migrate(
        &mut self,
        from: &NodeId,
        to: &NodeId,
        agent: &AgentId,
        env: DeployEnv,
    ) -> Result<(), String> {
        let i = self
            .index_of(from)
            .ok_or_else(|| format!("source node {} not found", from.0))?;
        let j = self
            .index_of(to)
            .ok_or_else(|| format!("target node {} not found", to.0))?;
        if i == j {
            return Ok(()); // already there — nothing to do.
        }
        let (src, dst) = if i < j {
            let (left, right) = self.nodes.split_at_mut(j);
            (&mut left[i], &mut right[0])
        } else {
            let (left, right) = self.nodes.split_at_mut(i);
            (&mut right[0], &mut left[j])
        };
        crate::migrate(src, dst, agent, env).map_err(|e| e.to_string())
    }

    /// Drain `agent` from every node except `keep` — the single-instance fence
    /// after a heal (the stale instance on the failed node is removed).
    fn drain_except(&mut self, agent: &AgentId, keep: &NodeId) {
        for n in self.nodes.iter_mut() {
            if n.id() != keep {
                n.take(agent);
            }
        }
    }

    fn checkpoint_of(&self, agent: &AgentId) -> Result<crate::Checkpoint, String> {
        self.nodes
            .iter()
            .find_map(|n| n.agent(agent))
            .map(|a| a.checkpoint())
            .ok_or_else(|| format!("agent {agent} not hosted"))
    }

    /// Start tracking `agent`'s self-update generations, taking its **current**
    /// state as the known-good baseline (generation 0).
    pub fn track(&mut self, agent: &AgentId) -> Result<(), String> {
        let cp = self.checkpoint_of(agent)?;
        self.histories
            .entry(agent.clone())
            .or_insert_with(|| CheckpointHistory::init(cp));
        Ok(())
    }

    /// Stage the agent's current (post-update) state as an unproven **candidate**
    /// generation. The verdict — keep it or restore the baseline — is the control
    /// plane's [`Decision::Promote`] / [`Decision::Rollback`]: exactly the
    /// "what counts as healthy" call `update.rs` left to its caller, learned in
    /// M5d over observed post-update reward.
    pub fn stage_update(&mut self, agent: &AgentId) -> Result<u64, String> {
        let cp = self.checkpoint_of(agent)?;
        self.histories
            .get_mut(agent)
            .ok_or_else(|| format!("agent {agent} not tracked (call track first)"))
            .map(|h| h.stage(cp))
    }

    /// The agent's self-update history, if tracked.
    pub fn history(&self, agent: &AgentId) -> Option<&CheckpointHistory> {
        self.histories.get(agent)
    }
}

/// One agent as the control plane sees it.
#[derive(Debug, Clone)]
pub struct AgentObs {
    pub id: AgentId,
    pub node: NodeId,
    pub health: Health,
    pub budget_remaining: u64,
    pub budget_total: u64,
    /// A staged self-update candidate awaits a promote-or-rollback verdict (M5d).
    pub candidate: bool,
    /// Work delivered per token, relative to the committed baseline — the
    /// **observed post-update reward** an update verdict is learned over
    /// (1.0 = baseline; supplied by throughput telemetry, or the simulator
    /// during training).
    pub observed_yield: f64,
    /// Ticks since the candidate was staged (0.0 when none) — how long the
    /// verdict has been gathering evidence.
    pub candidate_age: f64,
}

impl AgentObs {
    /// Remaining attention budget as a fraction of the ceiling (0.0–1.0).
    pub fn budget_frac(&self) -> f64 {
        if self.budget_total == 0 {
            0.0
        } else {
            self.budget_remaining as f64 / self.budget_total as f64
        }
    }
}

/// One node as the control plane sees it.
#[derive(Debug, Clone)]
pub struct NodeObs {
    pub id: NodeId,
    pub load: usize,
}

/// The full cluster observation a [`Policy`] reasons over (budget/health per
/// agent, load per node).
#[derive(Debug, Clone, Default)]
pub struct ClusterState {
    pub agents: Vec<AgentObs>,
    pub nodes: Vec<NodeObs>,
}

impl ClusterState {
    /// Fold the observation into the fixed-width [`StateVector`].
    pub fn vector(&self) -> StateVector {
        let n = self.agents.len();
        let n_suspected = self
            .agents
            .iter()
            .filter(|a| a.health == Health::Suspected)
            .count();
        let (mean_frac, min_frac) = if n == 0 {
            (1.0, 1.0)
        } else {
            let sum: f64 = self.agents.iter().map(AgentObs::budget_frac).sum();
            let min = self
                .agents
                .iter()
                .map(AgentObs::budget_frac)
                .fold(f64::INFINITY, f64::min);
            (sum / n as f64, min)
        };
        let max_load = self.nodes.iter().map(|nd| nd.load).max().unwrap_or(0);
        let min_load = self.nodes.iter().map(|nd| nd.load).min().unwrap_or(0);
        StateVector {
            n_agents: n as f64,
            n_suspected: n_suspected as f64,
            n_nodes: self.nodes.len() as f64,
            mean_budget_frac: mean_frac,
            min_budget_frac: min_frac,
            max_node_load: max_load as f64,
            min_node_load: min_load as f64,
            load_imbalance: (max_load - min_load) as f64,
        }
    }
}

/// A **fixed-width** numeric projection of the cluster — the *vector* the OS is
/// governed in (TAM §3). Its width ([`DIM`](StateVector::DIM)) is constant no
/// matter how many agents or nodes exist, so a future learned policy (M5c)
/// consumes a stable-shape input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StateVector {
    pub n_agents: f64,
    pub n_suspected: f64,
    pub n_nodes: f64,
    pub mean_budget_frac: f64,
    pub min_budget_frac: f64,
    pub max_node_load: f64,
    pub min_node_load: f64,
    pub load_imbalance: f64,
}

impl StateVector {
    /// The fixed dimensionality of the state vector.
    pub const DIM: usize = 8;

    /// The vector as a plain fixed-size array — a learned policy's input.
    pub fn as_vector(&self) -> [f64; Self::DIM] {
        [
            self.n_agents,
            self.n_suspected,
            self.n_nodes,
            self.mean_budget_frac,
            self.min_budget_frac,
            self.max_node_load,
            self.min_node_load,
            self.load_imbalance,
        ]
    }
}

/// What the policy proposes — each variant maps to exactly one existing mechanism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Restore a suspected agent's last-good checkpoint onto a healthy node
    /// (actuated by [`Supervisor::self_heal`]).
    Heal { agent: AgentId, onto: NodeId },
    /// Rebalance load: move an agent from an overloaded node onto a lighter one
    /// (actuated by runtime [`migrate`](crate::migrate)).
    Migrate {
        agent: AgentId,
        from: NodeId,
        to: NodeId,
    },
    /// Top up a starved-but-healthy agent's attention budget
    /// (actuated by [`Agent::grant_budget`]). The grant size is itself a policy
    /// output — M5d's learned adaptive-compute knob (RFC-0002 / RFC-0007 §5).
    Refill { agent: AgentId, tokens: u64 },
    /// Commit the agent's staged self-update candidate as the new known-good
    /// baseline (actuated by [`CheckpointHistory::promote`]) — the "healthy"
    /// verdict `update.rs` left to its caller, decided in M5d over observed
    /// post-update reward.
    Promote { agent: AgentId },
    /// Discard the staged candidate and restore the agent from its last
    /// committed generation (actuated by [`CheckpointHistory::rollback`] +
    /// [`Agent::restore`]) — the update regressed.
    Rollback { agent: AgentId },
}

impl Decision {
    /// The agent this decision governs — the target the actuation is
    /// capability-checked against (INV-2, RFC-0007 M5b).
    pub fn target(&self) -> &AgentId {
        match self {
            Decision::Heal { agent, .. }
            | Decision::Migrate { agent, .. }
            | Decision::Refill { agent, .. }
            | Decision::Promote { agent }
            | Decision::Rollback { agent } => agent,
        }
    }
}

/// The control plane's **policy** — the one swappable part (TAM §4.2). M5a ships
/// [`HeuristicPolicy`]; M5c replaces it with a learned `π_θ` over this same
/// [`ClusterState`] / [`StateVector`] interface, which it must out-perform on a
/// held-out suite before it may actuate.
pub trait Policy: Send + Sync {
    /// Propose decisions for the observed state (an empty vec = hold).
    fn decide(&self, state: &ClusterState) -> Vec<Decision>;
    /// Stable identifier for the audit ledger / telemetry.
    fn name(&self) -> &str;
}

/// The transparent hand-written baseline (M5a) — the **baseline of record** every
/// learned policy must beat. Rules, in priority order:
/// 1. **availability** — every `Suspected` agent is healed onto the lightest node
///    other than its own;
/// 2. **starvation** — every healthy agent under `low_budget_frac` is refilled;
/// 3. **balance** — if `max_load - min_load >= imbalance`, one healthy agent moves
///    off the busiest node onto the lightest.
pub struct HeuristicPolicy {
    /// Refill an agent whose remaining budget fraction drops below this.
    pub low_budget_frac: f64,
    /// Tokens granted per refill.
    pub refill_tokens: u64,
    /// Load gap (busiest − lightest) that triggers a rebalancing migration.
    pub imbalance: usize,
}

impl Default for HeuristicPolicy {
    fn default() -> Self {
        Self {
            low_budget_frac: 0.2,
            refill_tokens: 100,
            imbalance: 2,
        }
    }
}

impl HeuristicPolicy {
    pub fn new(low_budget_frac: f64, refill_tokens: u64, imbalance: usize) -> Self {
        Self {
            low_budget_frac,
            refill_tokens,
            imbalance,
        }
    }
}

impl Policy for HeuristicPolicy {
    fn decide(&self, state: &ClusterState) -> Vec<Decision> {
        let mut out = Vec::new();

        // 1. Availability: heal each suspected agent onto the lightest node that
        //    is not its own (fall back to the globally lightest if it is alone).
        for a in state
            .agents
            .iter()
            .filter(|a| a.health == Health::Suspected)
        {
            let onto = state
                .nodes
                .iter()
                .filter(|n| n.id != a.node)
                .min_by_key(|n| n.load)
                .or_else(|| state.nodes.iter().min_by_key(|n| n.load));
            if let Some(target) = onto {
                out.push(Decision::Heal {
                    agent: a.id.clone(),
                    onto: target.id.clone(),
                });
            }
        }

        // 2. Starvation: refill healthy agents under the budget floor.
        for a in state
            .agents
            .iter()
            .filter(|a| a.health == Health::Healthy && a.budget_frac() < self.low_budget_frac)
        {
            out.push(Decision::Refill {
                agent: a.id.clone(),
                tokens: self.refill_tokens,
            });
        }

        // 3. Balance: one migration off the busiest node if it is imbalanced.
        let hi = state.nodes.iter().max_by_key(|n| n.load);
        let lo = state.nodes.iter().min_by_key(|n| n.load);
        if let (Some(hi), Some(lo)) = (hi, lo)
            && hi.id != lo.id
            && hi.load.saturating_sub(lo.load) >= self.imbalance
            && let Some(a) = state
                .agents
                .iter()
                .find(|a| a.node == hi.id && a.health == Health::Healthy)
        {
            out.push(Decision::Migrate {
                agent: a.id.clone(),
                from: hi.id.clone(),
                to: lo.id.clone(),
            });
        }

        out
    }

    fn name(&self) -> &str {
        "heuristic"
    }
}

/// One decision and the outcome of actuating it.
#[derive(Debug, Clone)]
pub struct Actuation {
    pub decision: Decision,
    pub result: Result<(), String>,
}

/// The audit record of one control-plane [`tick`](ControlPlane::tick): the state
/// vector observed, and what the policy did about it (INV-4). This is also the
/// training datum a learned policy (M5c) consumes.
#[derive(Debug, Clone)]
pub struct StepReport {
    pub policy: String,
    pub vector: StateVector,
    pub actuations: Vec<Actuation>,
}

impl StepReport {
    /// How many decisions actuated successfully.
    pub fn applied(&self) -> usize {
        self.actuations.iter().filter(|a| a.result.is_ok()).count()
    }
}

/// The L4 governor: observe → policy → actuate, holding only the swappable
/// [`Policy`] and an audit history.
pub struct ControlPlane {
    policy: Box<dyn Policy>,
    history: Vec<StepReport>,
}

impl ControlPlane {
    pub fn new(policy: Box<dyn Policy>) -> Self {
        Self {
            policy,
            history: Vec::new(),
        }
    }

    /// The M5a baseline: a [`HeuristicPolicy`] with default thresholds.
    pub fn with_heuristic() -> Self {
        Self::new(Box::new(HeuristicPolicy::default()))
    }

    pub fn policy_name(&self) -> &str {
        self.policy.name()
    }

    /// The governor's audit trail (one [`StepReport`] per tick).
    pub fn history(&self) -> &[StepReport] {
        &self.history
    }

    /// Observe the cluster: live budget/load from `cluster`, health from
    /// `supervisor` (unregistered agents are assumed [`Healthy`](Health::Healthy)).
    pub fn observe(cluster: &Cluster, supervisor: &Supervisor) -> ClusterState {
        let mut agents = Vec::new();
        for node in cluster.nodes() {
            for a in node.agents() {
                let id = a.id().clone();
                let health = supervisor.health(&id).unwrap_or(Health::Healthy);
                let b = a.budget();
                let candidate = cluster
                    .history(&id)
                    .is_some_and(CheckpointHistory::has_candidate);
                agents.push(AgentObs {
                    id,
                    node: node.id().clone(),
                    health,
                    budget_remaining: b.remaining(),
                    budget_total: b.total,
                    candidate,
                    // Live throughput telemetry arrives with the fabric's yield
                    // reporting; until then the baseline is assumed.
                    observed_yield: 1.0,
                    candidate_age: 0.0,
                });
            }
        }
        let nodes = cluster
            .nodes()
            .iter()
            .map(|n| NodeObs {
                id: n.id().clone(),
                load: n.len(),
            })
            .collect();
        ClusterState { agents, nodes }
    }

    /// One control cycle: observe → decide → actuate. `env` supplies a fresh
    /// [`DeployEnv`] for each restore (heal/migrate rebind the environment).
    pub fn tick(
        &mut self,
        cluster: &mut Cluster,
        supervisor: &mut Supervisor,
        env: &dyn Fn() -> DeployEnv,
    ) -> StepReport {
        let state = Self::observe(cluster, supervisor);
        let vector = state.vector();
        let decisions = self.policy.decide(&state);
        let mut actuations = Vec::new();
        for decision in decisions {
            let result = actuate(&decision, cluster, supervisor, env);
            actuations.push(Actuation { decision, result });
        }
        let report = StepReport {
            policy: self.policy.name().to_string(),
            vector,
            actuations,
        };
        self.history.push(report.clone());
        report
    }
}

/// Apply one decision through its mechanism — the shared actuation path for the
/// M5a [`ControlPlane`] and the M5b [`Governor`]. Best-effort: an inapplicable
/// decision returns `Err(String)`.
fn actuate(
    decision: &Decision,
    cluster: &mut Cluster,
    supervisor: &mut Supervisor,
    env: &dyn Fn() -> DeployEnv,
) -> Result<(), String> {
    match decision {
        Decision::Heal { agent, onto } => {
            let node = cluster
                .node_mut(onto)
                .ok_or_else(|| format!("node {} not found", onto.0))?;
            supervisor
                .self_heal(agent, node, env())
                .map_err(|e| e.to_string())?;
            cluster.drain_except(agent, onto); // single-instance fence
            Ok(())
        }
        Decision::Migrate { agent, from, to } => cluster.migrate(from, to, agent, env()),
        Decision::Refill { agent, tokens } => {
            let node_id = cluster
                .nodes()
                .iter()
                .find(|n| n.hosts(agent))
                .map(|n| n.id().clone())
                .ok_or_else(|| format!("agent {agent} not hosted"))?;
            cluster
                .node_mut(&node_id)
                .and_then(|n| n.agent_mut(agent))
                .expect("located above")
                .grant_budget(*tokens);
            Ok(())
        }
        Decision::Promote { agent } => cluster
            .histories
            .get_mut(agent)
            .ok_or_else(|| format!("agent {agent} not tracked"))?
            .promote()
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Decision::Rollback { agent } => {
            let node_id = cluster
                .nodes()
                .iter()
                .find(|n| n.hosts(agent))
                .map(|n| n.id().clone())
                .ok_or_else(|| format!("agent {agent} not hosted"))?;
            let last_good = {
                let h = cluster
                    .histories
                    .get_mut(agent)
                    .ok_or_else(|| format!("agent {agent} not tracked"))?;
                if !h.has_candidate() {
                    return Err(format!("no candidate staged for {agent}"));
                }
                h.rollback();
                h.last_good().clone()
            };
            let e = env();
            let restored =
                Agent::restore(&last_good, e.memory, e.mind).map_err(|err| err.to_string())?;
            let slot = cluster
                .node_mut(&node_id)
                .and_then(|n| n.agent_mut(agent))
                .expect("located above");
            *slot = restored;
            Ok(())
        }
    }
}

// ───────────────────────── M5b: the governor as an agent ─────────────────────

/// How aggressively the governor actuates — gated **in-system**, never by a human
/// (INV-5). A learned policy (M5c) is promoted `Shadow → Canary → Act` by the
/// plane itself once it clears its falsification gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Decide and log, but never actuate — where a policy proves itself first.
    Shadow,
    /// Actuate at most `n` decisions this tick — a bounded, revertible blast radius.
    Canary(usize),
    /// Actuate every authorized decision.
    Act,
}

impl Mode {
    /// Whether this mode permits actuating once `applied` decisions already have.
    fn permits(self, applied: usize) -> bool {
        match self {
            Mode::Shadow => false,
            Mode::Canary(limit) => applied < limit,
            Mode::Act => true,
        }
    }
}

/// What became of one decision under the governor's gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Applied through its mechanism.
    Applied,
    /// Held back by the [`Mode`] (shadow, or beyond the canary limit).
    Shadowed,
    /// Denied — the governor holds no capability over the target (INV-2).
    Denied,
    /// The mechanism returned an error.
    Failed(String),
}

/// One decision and what the governor did with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovDecision {
    pub decision: Decision,
    pub disposition: Disposition,
}

/// The audit record of one [`Governor`] tick (INV-4) — including the governor's
/// own budget spend (INV-1) and per-decision disposition.
#[derive(Debug, Clone)]
pub struct GovReport {
    pub governor: AgentId,
    pub mode: Mode,
    pub vector: StateVector,
    /// The governor's own remaining budget after deliberating (INV-1).
    pub budget_remaining: u64,
    /// `false` ⇒ the governor was too starved to even think this tick.
    pub deliberated: bool,
    pub decisions: Vec<GovDecision>,
}

impl GovReport {
    /// How many decisions actuated successfully.
    pub fn applied(&self) -> usize {
        self.decisions
            .iter()
            .filter(|d| d.disposition == Disposition::Applied)
            .count()
    }
}

/// The **agentic** control plane (M5b): the governor is itself a first-class
/// [`Agent`]. It *thinks* over the state vector (spending budget, INV-1), *acts
/// under capability* (INV-2) on every actuation, and is *audited* (INV-4) —
/// entirely in-system, with no human in the loop and no authority above it
/// (INV-5). Actuation is gated by a [`Mode`] (shadow → canary → act) the plane
/// sets for itself.
///
/// "AI manages AI" is therefore literal: the governor obeys the same TAM contract
/// as the agents it governs, and the only thing it answers to is its purpose.
pub struct Governor {
    agent: Agent,
    policy: Box<dyn Policy>,
    mode: Mode,
    think_cost: u64,
    history: Vec<GovReport>,
}

impl Governor {
    /// Build a governor from its **own** agent (identity + budget + capabilities;
    /// must be live), a policy, and a starting mode. `think_cost` tokens are
    /// charged per tick to deliberate.
    pub fn new(agent: Agent, policy: Box<dyn Policy>, mode: Mode, think_cost: u64) -> Self {
        Self {
            agent,
            policy,
            mode,
            think_cost,
            history: Vec::new(),
        }
    }

    pub fn id(&self) -> &AgentId {
        self.agent.id()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Change the actuation mode — an in-system decision (e.g. promote a policy
    /// `Shadow → Canary` once it clears its gate). No human approval (INV-5).
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// The governor's own remaining attention budget (INV-1).
    pub fn budget_remaining(&self) -> u64 {
        self.agent.remaining_budget()
    }

    /// The governor's own audit trail (INV-4) — the governor is as inspectable as
    /// the governed.
    pub fn audit(&self) -> &[AuditRecord] {
        self.agent.audit()
    }

    pub fn history(&self) -> &[GovReport] {
        &self.history
    }

    /// One agentic control cycle: **think** (pay budget) → observe → decide →
    /// gate (capability + mode) → actuate. If the governor cannot afford to think,
    /// it governs nothing this tick — it cannot livelock the fleet by deliberating
    /// for free.
    pub async fn tick(
        &mut self,
        cluster: &mut Cluster,
        supervisor: &mut Supervisor,
        env: &dyn Fn() -> DeployEnv,
    ) -> GovReport {
        let state = ControlPlane::observe(cluster, supervisor);
        let vector = state.vector();

        // INV-1: deliberation is metered work. If the budget can't cover a Think,
        // the governor is starved and actuates nothing this tick.
        let deliberated = self
            .agent
            .act(Action::Think {
                prompt: format!(
                    "govern: {} agents, {} suspected",
                    vector.n_agents as u64, vector.n_suspected as u64
                ),
                cost: self.think_cost,
            })
            .await
            .is_ok();

        let mut decisions = Vec::new();
        if deliberated {
            let mut applied = 0usize;
            for decision in self.policy.decide(&state) {
                let target = decision.target().as_str().to_string();
                let disposition =
                    if !self
                        .agent
                        .can(Permission::Admin, ResourceKind::Agent, &target)
                    {
                        // INV-2: the governor needs a capability over the target.
                        Disposition::Denied
                    } else if !self.mode.permits(applied) {
                        Disposition::Shadowed
                    } else {
                        match actuate(&decision, cluster, supervisor, env) {
                            Ok(()) => {
                                applied += 1;
                                Disposition::Applied
                            }
                            Err(e) => Disposition::Failed(e),
                        }
                    };
                decisions.push(GovDecision {
                    decision,
                    disposition,
                });
            }
        }

        let report = GovReport {
            governor: self.agent.id().clone(),
            mode: self.mode,
            vector,
            budget_remaining: self.agent.remaining_budget(),
            deliberated,
            decisions,
        };
        self.history.push(report.clone());
        report
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use thaliox_cognition::MockProvider;
    use thaliox_core::{
        AgentId, AttentionBudget, CapabilityToken, Permission, ResourceKind, Scope,
    };
    use thaliox_memory::InMemorySpace;

    use super::*;

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

    async fn agent_with_spend(id: &str, total: u64, thinks: usize) -> Agent {
        let mut a = live_agent(id, total);
        for _ in 0..thinks {
            a.act(Action::Think {
                prompt: "w".into(),
                cost: 5,
            })
            .await
            .unwrap();
        }
        a
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[tokio::test]
    async fn heuristic_heals_a_suspected_agent() {
        let a = agent_with_spend("a1", 100, 1).await; // budget 95
        let id = a.id().clone();
        let mut cluster = Cluster::new();
        let mut node_a = Node::new("A");
        node_a.host(a);
        cluster.add(node_a);
        cluster.add(Node::new("B"));

        let mut sup = Supervisor::new(2);
        let cp_a = cluster.node(&NodeId::new("A")).unwrap();
        sup.observe(&id, NodeId::new("A"), cp_a.agent(&id).unwrap().checkpoint());
        sup.tick();
        sup.tick();
        assert_eq!(sup.health(&id), Some(Health::Suspected));

        let mut cp = ControlPlane::with_heuristic();
        let report = cp.tick(&mut cluster, &mut sup, &fresh_env);

        assert!(matches!(
            report.actuations[0].decision,
            Decision::Heal { .. }
        ));
        assert_eq!(report.applied(), 1);
        // Healed onto B (lightest, not its own node A); A drained; registry → B.
        assert!(cluster.node(&NodeId::new("B")).unwrap().hosts(&id));
        assert!(cluster.node(&NodeId::new("A")).unwrap().is_empty());
        assert_eq!(sup.node_of(&id), Some(&NodeId::new("B")));
        let healed = cluster.node(&NodeId::new("B")).unwrap().agent(&id).unwrap();
        assert_eq!(healed.remaining_budget(), 95); // state continued, not reset
        assert_eq!(cp.history().len(), 1);
    }

    #[tokio::test]
    async fn heuristic_refills_a_starved_agent() {
        // total 100, spend 85 (17 thinks) → remaining 15, frac 0.15 < 0.2.
        let a = agent_with_spend("a1", 100, 17).await;
        let id = a.id().clone();
        let mut cluster = Cluster::new();
        let mut node = Node::new("A");
        node.host(a);
        cluster.add(node);

        let mut sup = Supervisor::new(2); // a1 unregistered → Healthy
        let mut cp = ControlPlane::with_heuristic(); // refill 100
        let report = cp.tick(&mut cluster, &mut sup, &fresh_env);

        assert!(matches!(
            report.actuations[0].decision,
            Decision::Refill { tokens: 100, .. }
        ));
        let agent = cluster.node(&NodeId::new("A")).unwrap().agent(&id).unwrap();
        // ceiling +100 → remaining 15 + 100 = 115.
        assert_eq!(agent.remaining_budget(), 115);
    }

    #[test]
    fn heuristic_rebalances_load() {
        let mut cluster = Cluster::new();
        let mut a = Node::new("A");
        a.host(live_agent("a1", 100));
        a.host(live_agent("a2", 100));
        a.host(live_agent("a3", 100));
        cluster.add(a);
        cluster.add(Node::new("B")); // empty → imbalance 3 ≥ 2

        let mut sup = Supervisor::new(2);
        let mut cp = ControlPlane::with_heuristic();
        let report = cp.tick(&mut cluster, &mut sup, &fresh_env);

        assert!(
            report
                .actuations
                .iter()
                .any(|a| matches!(a.decision, Decision::Migrate { .. }) && a.result.is_ok())
        );
        assert_eq!(cluster.node(&NodeId::new("A")).unwrap().len(), 2);
        assert_eq!(cluster.node(&NodeId::new("B")).unwrap().len(), 1);
    }

    #[test]
    fn healthy_balanced_cluster_holds() {
        let mut cluster = Cluster::new();
        let mut a = Node::new("A");
        a.host(live_agent("a1", 100));
        cluster.add(a);
        let mut b = Node::new("B");
        b.host(live_agent("a2", 100));
        cluster.add(b);

        let mut sup = Supervisor::new(2);
        let mut cp = ControlPlane::with_heuristic();
        let report = cp.tick(&mut cluster, &mut sup, &fresh_env);

        assert!(report.actuations.is_empty());
        assert!(approx(report.vector.load_imbalance, 0.0));
    }

    #[test]
    fn state_vector_is_fixed_width_regardless_of_size() {
        let small = ClusterState {
            agents: vec![],
            nodes: vec![NodeObs {
                id: NodeId::new("A"),
                load: 0,
            }],
        };
        let big = ClusterState {
            agents: (0..50)
                .map(|i| AgentObs {
                    id: AgentId::new(format!("a{i}")),
                    node: NodeId::new("A"),
                    health: Health::Healthy,
                    budget_remaining: 50,
                    budget_total: 100,
                    candidate: false,
                    observed_yield: 1.0,
                    candidate_age: 0.0,
                })
                .collect(),
            nodes: (0..10)
                .map(|i| NodeObs {
                    id: NodeId::new(format!("n{i}")),
                    load: i,
                })
                .collect(),
        };
        assert_eq!(small.vector().as_vector().len(), StateVector::DIM);
        assert_eq!(big.vector().as_vector().len(), StateVector::DIM);
        assert_eq!(StateVector::DIM, 8);

        let v = big.vector();
        assert!(approx(v.n_agents, 50.0));
        assert!(approx(v.mean_budget_frac, 0.5));
        assert!(approx(v.load_imbalance, 9.0)); // loads 0..=9
    }

    /// The policy is the single swap point: the same suspected-agent cluster, run
    /// under a policy that holds, produces zero actuation.
    struct HoldPolicy;
    impl Policy for HoldPolicy {
        fn decide(&self, _: &ClusterState) -> Vec<Decision> {
            Vec::new()
        }
        fn name(&self) -> &str {
            "hold"
        }
    }

    #[tokio::test]
    async fn policy_is_the_swap_point() {
        let a = agent_with_spend("a1", 100, 1).await;
        let id = a.id().clone();
        let mut cluster = Cluster::new();
        let mut node_a = Node::new("A");
        node_a.host(a);
        cluster.add(node_a);
        cluster.add(Node::new("B"));

        let mut sup = Supervisor::new(2);
        let cp_a = cluster.node(&NodeId::new("A")).unwrap();
        sup.observe(&id, NodeId::new("A"), cp_a.agent(&id).unwrap().checkpoint());
        sup.tick();
        sup.tick();
        assert_eq!(sup.health(&id), Some(Health::Suspected));

        let mut cp = ControlPlane::new(Box::new(HoldPolicy));
        let report = cp.tick(&mut cluster, &mut sup, &fresh_env);

        assert_eq!(cp.policy_name(), "hold");
        assert!(report.actuations.is_empty());
        // Not healed — the mechanism only runs when the policy says so.
        assert!(cluster.node(&NodeId::new("A")).unwrap().hosts(&id));
        assert!(cluster.node(&NodeId::new("B")).unwrap().is_empty());
    }

    /// M5d mechanism path: a staged self-update candidate is concluded by the
    /// control plane — `Promote` commits it; `Rollback` discards it and restores
    /// the agent from the last committed generation. Same swap point, same
    /// actuation pipeline, real `update.rs` underneath.
    struct Verdict(Decision);
    impl Policy for Verdict {
        fn decide(&self, _: &ClusterState) -> Vec<Decision> {
            vec![self.0.clone()]
        }
        fn name(&self) -> &str {
            "verdict"
        }
    }

    #[tokio::test]
    async fn update_verdicts_actuate_through_the_real_mechanism() {
        let a = live_agent("a1", 100);
        let id = a.id().clone();
        let mut node = Node::new("A");
        node.host(a);
        let mut cluster = Cluster::new().with_node(node);
        let mut sup = Supervisor::new(2);

        // Track the known-good baseline (budget 100), then "apply an update"
        // (metered work — budget drops to 95) and stage it as a candidate.
        cluster.track(&id).unwrap();
        cluster
            .node_mut(&NodeId::new("A"))
            .unwrap()
            .agent_mut(&id)
            .unwrap()
            .act(Action::Think {
                prompt: "v2".into(),
                cost: 5,
            })
            .await
            .unwrap();
        cluster.stage_update(&id).unwrap();
        assert!(cluster.history(&id).unwrap().has_candidate());

        // The staged candidate is visible to the policy (AgentObs.candidate).
        let state = ControlPlane::observe(&cluster, &sup);
        assert!(state.agents[0].candidate);

        // Verdict: the update regressed → Rollback restores the baseline.
        let mut cp = ControlPlane::new(Box::new(Verdict(Decision::Rollback { agent: id.clone() })));
        let report = cp.tick(&mut cluster, &mut sup, &fresh_env);
        assert_eq!(report.applied(), 1);
        let h = cluster.history(&id).unwrap();
        assert!(!h.has_candidate());
        assert_eq!(h.current_gen(), 0);
        let restored = cluster.node(&NodeId::new("A")).unwrap().agent(&id).unwrap();
        assert_eq!(restored.remaining_budget(), 100); // pre-update state is back

        // Second round: stage again and this time commit it as the new baseline.
        cluster
            .node_mut(&NodeId::new("A"))
            .unwrap()
            .agent_mut(&id)
            .unwrap()
            .act(Action::Think {
                prompt: "v3".into(),
                cost: 5,
            })
            .await
            .unwrap();
        cluster.stage_update(&id).unwrap();
        let mut cp = ControlPlane::new(Box::new(Verdict(Decision::Promote { agent: id.clone() })));
        let report = cp.tick(&mut cluster, &mut sup, &fresh_env);
        assert_eq!(report.applied(), 1);
        let h = cluster.history(&id).unwrap();
        assert!(!h.has_candidate());
        assert_eq!(h.current_gen(), 2); // gen2 committed (gen1 was rolled back)
        // A rollback verdict with nothing staged is refused by the mechanism.
        let mut cp = ControlPlane::new(Box::new(Verdict(Decision::Rollback { agent: id.clone() })));
        let report = cp.tick(&mut cluster, &mut sup, &fresh_env);
        assert_eq!(report.applied(), 0);
    }

    // ───────────────────────────── M5b: the governor ─────────────────────────

    /// A two-node cluster (A holds the named agents, B is empty) where every
    /// named agent has been driven to `Suspected`.
    async fn suspected_cluster(ids: &[&str]) -> (Cluster, Supervisor) {
        let mut node_a = Node::new("A");
        let mut sup = Supervisor::new(2);
        for &id in ids {
            let a = agent_with_spend(id, 100, 1).await; // budget 95, healthy frac
            sup.observe(&AgentId::new(id), NodeId::new("A"), a.checkpoint());
            node_a.host(a);
        }
        let mut cluster = Cluster::new();
        cluster.add(node_a);
        cluster.add(Node::new("B"));
        sup.tick();
        sup.tick(); // miss_threshold 2 → all Suspected
        (cluster, sup)
    }

    /// An `Admin`-over-`Agent` capability scoped to `pattern` (the `govern.*` grant).
    fn govern_cap(pattern: &str) -> CapabilityToken {
        CapabilityToken {
            subject: AgentId::new("gov"),
            permissions: vec![Permission::Admin],
            scope: vec![Scope {
                resource: ResourceKind::Agent,
                pattern: pattern.into(),
            }],
            issued_at: 0,
            expires_at: 0,
            jti: [0; 16],
            delegable: false,
            signature: [0; 32],
        }
    }

    fn governor(budget: u64, mode: Mode, cap: Option<&str>) -> Governor {
        let mut a = live_agent("gov", budget);
        if let Some(p) = cap {
            a = a.grant(govern_cap(p));
        }
        Governor::new(a, Box::new(HeuristicPolicy::default()), mode, 5)
    }

    #[tokio::test]
    async fn governor_thinks_and_acts_under_capability() {
        let (mut cluster, mut sup) = suspected_cluster(&["a1"]).await;
        let mut gov = governor(1000, Mode::Act, Some("*"));
        let report = gov.tick(&mut cluster, &mut sup, &fresh_env).await;

        assert!(report.deliberated);
        assert_eq!(report.budget_remaining, 995); // Think cost 5 (INV-1)
        assert!(!gov.audit().is_empty()); // the Think is audited (INV-4)
        assert_eq!(report.applied(), 1);
        assert_eq!(report.decisions[0].disposition, Disposition::Applied);
        assert!(
            cluster
                .node(&NodeId::new("B"))
                .unwrap()
                .hosts(&AgentId::new("a1"))
        );
    }

    #[tokio::test]
    async fn governor_without_capability_is_denied() {
        // INV-2: a govern cap scoped to "other-*" does not cover agent "a1".
        let (mut cluster, mut sup) = suspected_cluster(&["a1"]).await;
        let mut gov = governor(1000, Mode::Act, Some("other-*"));
        let report = gov.tick(&mut cluster, &mut sup, &fresh_env).await;

        assert!(report.deliberated);
        assert_eq!(report.decisions[0].disposition, Disposition::Denied);
        assert_eq!(report.applied(), 0);
        // Denied at the door — not healed.
        assert!(
            cluster
                .node(&NodeId::new("A"))
                .unwrap()
                .hosts(&AgentId::new("a1"))
        );
        assert!(cluster.node(&NodeId::new("B")).unwrap().is_empty());
    }

    #[tokio::test]
    async fn shadow_mode_decides_but_does_not_actuate() {
        let (mut cluster, mut sup) = suspected_cluster(&["a1"]).await;
        let mut gov = governor(1000, Mode::Shadow, Some("*"));
        let report = gov.tick(&mut cluster, &mut sup, &fresh_env).await;

        assert!(report.deliberated);
        assert_eq!(report.decisions.len(), 1); // it DID decide
        assert_eq!(report.decisions[0].disposition, Disposition::Shadowed);
        assert_eq!(report.applied(), 0);
        // Shadow observes without touching the fleet.
        assert!(
            cluster
                .node(&NodeId::new("A"))
                .unwrap()
                .hosts(&AgentId::new("a1"))
        );
    }

    #[tokio::test]
    async fn canary_mode_bounds_the_blast_radius() {
        // Two suspected agents → two heal decisions; Canary(1) applies exactly one.
        let (mut cluster, mut sup) = suspected_cluster(&["a1", "a2"]).await;
        let mut gov = governor(1000, Mode::Canary(1), Some("*"));
        let report = gov.tick(&mut cluster, &mut sup, &fresh_env).await;

        assert_eq!(report.decisions.len(), 2);
        assert_eq!(report.applied(), 1);
        assert_eq!(
            report
                .decisions
                .iter()
                .filter(|d| d.disposition == Disposition::Shadowed)
                .count(),
            1
        );
        assert_eq!(cluster.node(&NodeId::new("B")).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn starved_governor_governs_nothing() {
        // INV-1: think_cost 5 > budget 3 → can't deliberate → no governance.
        let (mut cluster, mut sup) = suspected_cluster(&["a1"]).await;
        let mut gov = governor(3, Mode::Act, Some("*"));
        let report = gov.tick(&mut cluster, &mut sup, &fresh_env).await;

        assert!(!report.deliberated);
        assert!(report.decisions.is_empty());
        // The fleet is untouched when the governor can't afford to think.
        assert!(
            cluster
                .node(&NodeId::new("A"))
                .unwrap()
                .hosts(&AgentId::new("a1"))
        );
        assert!(cluster.node(&NodeId::new("B")).unwrap().is_empty());
    }
}
