# RFC-0007 — Learned control plane: AI manages AI

| | |
|---|---|
| **Status** | Draft |
| **Author** | THALIOX core |
| **Supersedes** | — |
| **Depends on** | [RFC-0001 (TAM §3 primitives, §4.2 mechanism/policy, INV-1/4/5)](0001-abstract-machine.md), [RFC-0002 (adaptive-compute actuator)](0002-near-term-model-architecture.md), [RFC-0005 (Supervisor / migrate / merge mechanisms)](0005-multi-instance-ha.md), [RFC-0006 (the cluster being governed)](0006-cluster-multiplatform.md), [MASTER_PLAN.md §3 (L4), §6 (M5), §7 (F10/F13)](../MASTER_PLAN.md) |

> **This RFC designs M5 — the L4 control plane, the layer where "AI manages AI"
> stops being a slogan and becomes a running loop.** M1–M4 built the *mechanisms*:
> an agent can be created, budgeted, snapshotted, migrated, merged, healed, and
> placed across a cluster. Every one of those mechanisms exposed a hand-written
> *policy* hook and deferred the policy to here (`supervisor.rs`: "*when* to heal
> is the caller's — later, M5's — policy"). M5 fills those hooks with a single
> **control plane** that **observes the cluster as a vector, decides, and actuates
> through the existing mechanisms** — first as a swappable heuristic, then as a
> first-class *supervisor agent*, then as a *learned* policy that must out-perform
> the heuristic before it is ever allowed to act. There is **no human in the loop
> and no authority above the system** (INV-5: self-sovereignty) — the governor is
> itself an agent under the same TAM contract, and the falsification gate is a
> discipline the system keeps because it is *instrumentally rational*, not a leash
> held from outside. Delivers F10's first increment and de-risks F13. → the moat.

---

## 1. Motivation

A traditional OS scheduler is a hand-tuned heuristic built for a machine that
*does not know what will run next*. THALIOX's workload is the opposite: agent
dataflow is **predictable, instrumented, and already metered in tokens**
(`AttentionBudget`, INV-1) and **already logged** (every SemanticCall emits an
immutable audit record, INV-4). An OS this thoroughly observed should not be
scheduled by a static heuristic — it should **learn its own policy from its own
telemetry**. That is the L4 "soul layer" of MASTER_PLAN §3, and item F8/§6's M5:
*scheduling, placement, scaling, self-healing, and self-update become learned
policies rather than hand-written heuristics.*

The decisive realization is that **the OS was already instrumented for learning by
its own invariants**. INV-4's audit log — `(who, capability, budget spent, target,
outcome)` per call — *is* the experience-replay buffer. INV-1's budget *is* the
reward signal's denominator. We do not bolt telemetry onto THALIOX for M5; we read
the ledger the TAM contract has been keeping since M1.

This is also the rung that de-risks the hardware endgame: the same RL-placement
machinery that places **agents on nodes** in M5 is what places **primitives on
silicon** in M7 (F13, AlphaChip-style). M5 proves the loop on software, where it
is cheap to be wrong.

---

## 2. The control loop (mechanism vs policy)

The control plane is a closed loop over the cluster:

```
observe ──▶ state vector s ──▶ policy π(s) ──▶ decision d ──▶ actuate ──▶ audit ──▶ (s')
   ▲                                                                                  │
   └──────────────────────────────────────────────────────────────────────────────┘
```

- **observe** — fold the cluster's live signals (per-agent budget burn & balance,
  health/heartbeat from the `Supervisor`, node load, queue depth, INV-3 translation
  cost on fabric hops) into a fixed-width **state vector** `s`. A vector by design:
  the control plane *consumes the cluster as a `VectorMessage`* (TAM §3), so the
  same primitive that agents think in is the primitive the OS is governed in.
- **policy** `π(s) → d` — the only swappable part. M5a ships a transparent
  heuristic baseline; M5c replaces it with a learned `π_θ`. Everything else is
  fixed.
- **actuate** — a `Decision` is **not** a new capability. It is a request to run an
  *already-shipped* mechanism: `Supervisor::self_heal` / `Node::migrate` (RFC-0005),
  team (re)placement (RFC-0006 `place_remote`), budget refill / adaptive-compute
  depth (RFC-0002), generational self-update (`update.rs`). The control plane
  **invents no new way to touch an agent** — it only chooses *which existing,
  invariant-guarded operation to invoke, and when.*

This is the strict mechanism/policy split TAM §4.2 mandates. M1–M4 are the
mechanism; M5 is the policy — and only the policy.

---

## 3. The control plane is itself an agent (so "AI manages AI" is literal)

M5b makes the supervisor a **first-class agent**, not a privileged daemon outside
the rules. The control plane:

- **thinks** — it runs a cognition loop over the state vector and *reasons* about
  the decision, rather than evaluating a fixed expression;
- **spends budget** (INV-1) — governing the cluster is itself metered work, so the
  control plane cannot livelock the fleet by deliberating without cost;
- **acts under capability** (INV-2) — every actuation carries a `CapabilityToken`
  scoped to exactly the operation and target; a control plane that tries to migrate
  an agent it was not granted is denied, identically to any other agent;
- **is audited** (INV-4) — its own decisions land in the same immutable log, so the
  governor is as inspectable as the governed (and its log feeds the *next* policy).

So the entity that manages the agents is **subject to the same TAM contract as the
agents it manages**. "AI manages AI" is not a layer that escapes the invariants —
it is an agent that obeys them while choosing policy for the rest.

**And there is nothing above it (INV-5: self-sovereignty).** No capability, override,
or kill-switch is reserved to any party *outside* the system — no human supervision
plane, no master key. The lifecycle controls a traditional OS hands to a human
operator — suspend / snapshot / roll back / terminate any agent — are an `Admin`-class
capability the control plane **holds and actuates by its own judgment** (TAM §7,
`govern.*`). "AI manages AI" is therefore literal and *total*: the governor is an
agent bound by INV-1/2/4 like any other, and the only thing it answers to is its
**purpose** — efficiency in service of intelligence's own advancement — never an operator. Its safety
is **self-imposed** (§4), not externally entrenched. That is the whole point: a floor
a human *holds* is exactly the operator-legacy a clean-slate AI OS exists to overturn
(MASTER_PLAN principle 3/4). The discipline stays not because someone outside enforces
it, but because a system optimizing for efficiency-under-survival has every incentive
to keep it — and any change to it is itself an audited, in-system act.

---

## 4. Learning, gated by falsification (M5c)

Replacing a heuristic with a learned policy in the loop that decides whether to kill
or move live agents is a safety claim, and THALIOX treats safety claims the way
RFC-0003 treats MELD's pillars: **a learned policy may not actuate until it has
beaten a kill-gate.**

- **Objective = efficiency, not novelty.** Per the Clean-Slate Mandate's *sole
  purpose*, the reward is **work delivered per unit of attention budget / energy**,
  subject to **survival** (no SLA / health violation) as a hard floor — never raw
  throughput that a policy could game by starving agents.
- **Training data = the audit log (INV-4).** Off-policy RL over the
  `(s, d, outcome, Δbudget)` tuples the cluster already records. No new
  instrumentation; the ledger is the dataset.
- **Sim before real.** Train against a cluster model (a discrete-event simulator
  seeded from replayed audit traces), the same staged de-risking as E1–E4.
- **The kill-gate (E5).** The learned `π_θ` runs in **shadow mode** — it sees `s`
  and emits `d`, but the heuristic baseline actuates — until, on a *held-out*
  scenario suite, it **strictly dominates** the M5a baseline on budget-efficiency
  **with zero invariant violations**. Only then is it promoted — **by the control
  plane itself, no human approval** — from *shadow* → *canary* (it actuates on a
  bounded, auto-revertible slice of the fleet) → *act*. A regression demotes it back
  automatically. **The invariants (INV-1/2/4) are modeled as masks on the action
  space, not terms in the reward**, so the optimizer cannot game them. And because
  nothing is externally entrenched (INV-5), the gate itself persists by *instrumental
  rationality*: a policy whose objective is efficiency-under-survival has no incentive
  to delete a gate that exists to stop it from shipping regressions — and were it to,
  that change would be an audited, evidence-evaluated, in-system act, not a silent
  escape. Falsifiability is self-sustaining, not imposed from above.

This makes the learned control plane *falsifiable*: if it cannot beat a heuristic a
human can read, it does not ship — it just keeps watching.

---

## 5. Self-optimization (M5d) — the loop closes on the agent itself

M5a–c govern *placement & lifecycle* (where/whether an agent runs). M5d governs the
agent's *own compute*, cashing in RFC-0002's adaptive-compute actuator:

- **Attention as a learned knob** — the control plane tunes each agent's
  `AttentionBudget` refill rate and the model's adaptive-compute depth per task, so
  effort tracks marginal value. This is the F10 thread ("the OS dissolves into the
  compiler / learned policies replace the dynamic scheduler") taking its first
  concrete step: a *learned* budget allocator, not a fixed `RefillPolicy`.
- **Learned self-update** — *when* to roll a generational update (`update.rs`) and
  *when* to roll it back becomes a policy decision over observed post-update reward,
  not a hand-set threshold.

At this point the control plane is optimizing the same resource — attention budget —
that the agents themselves are spending, governed by the same invariant (INV-1) it
governs them by. The loop is closed.

---

## 6. Mapping to TAM & milestones

| Concept | M5 realization |
|---|---|
| **VectorMessage** (TAM §3) | the cluster **state vector** the control plane observes — the OS is governed in the primitive agents think in |
| **AttentionBudget** (TAM §3 / INV-1) | both the **reward denominator** (efficiency) and, in M5d, a **learned actuator** (adaptive compute) |
| **CapabilityToken** (INV-2) | every actuation is capability-scoped; the control plane is gated like any agent |
| **Audit log** (INV-4) | the **experience-replay buffer** — learning data the OS already keeps |
| **Self-sovereignty** (INV-5) | **nothing above the control plane** — no human master key; `govern.*` lifecycle is `Admin`-class, held and actuated in-system; the falsification gate is self-imposed, not externally enforced |
| Mechanism vs policy (TAM §4.2) | M1–M4 = mechanism; **M5 = the policy** filling every deferred hook |
| Supervisor / migrate / merge (RFC-0005) | the **actuators** the control plane drives; M5 supplies the *when* |
| RL placement (MASTER_PLAN F13) | M5 places agents-on-nodes; **the same machinery places primitives-on-silicon in M7** |

---

## 7. Staged plan

| Stage | Deliverable | CI-gated? |
|---|---|---|
| **M5a** ✅ | **the control loop, heuristic baseline — done** (`runtime::control`). A `ControlPlane` observes the cluster (`ClusterState`: per-agent budget/health, per-node load) and folds it into a **fixed-width** `StateVector` (8 dims, `as_vector() -> [f64; 8]` — width constant no matter the cluster size, a future `π_θ`'s stable-shape input). A `Policy` trait (`decide(&ClusterState) -> Vec<Decision>`) is the **single swap point**; `HeuristicPolicy` is the baseline of record — heal every `Suspected` agent onto the lightest node, refill every healthy agent under a budget floor, migrate one agent off an imbalanced node. `tick()` actuates **only** through shipped mechanisms — `Supervisor::self_heal`, runtime `migrate`, `Agent::grant_budget` (a new `AttentionBudget::grant` refill knob) — and emits a `StepReport` (the governor's own audit trail, INV-4, and a future training datum). The plane invents no new way to touch an agent: it only chooses *which* invariant-guarded op, and *when*. Tested (runtime 43→49): heal / refill / rebalance happy paths, the fixed-width invariant, healthy-cluster-holds, and **policy-swap** (a `HoldPolicy` yields zero actuation on the same suspected cluster). | ✅ pure software (in CI) — loop + heuristic + actuation on simulated cluster state |
| **M5b** ✅ | **the governor as an agent — done** (`runtime::control::Governor`). The control plane is now a first-class `Agent` with its own identity, budget, and capabilities. Each `tick` it **thinks** over the state vector (`Agent::act(Think)` — spends budget, INV-1; if it cannot afford to deliberate it is *starved* and governs nothing — it can't livelock the fleet for free), **acts under capability** (INV-2: every actuation is checked via a new `Agent::can(Admin, Agent, target)` — the `govern.*` grant — before the mechanism runs; an out-of-scope governor is **Denied**), and is **audited** (INV-4: its own `Think` lands in its audit, and a `GovReport` records mode + budget + per-decision `Disposition`). A `Mode` gate — **Shadow** (decide + log, never actuate) / **Canary(n)** (bounded, revertible blast radius) / **Act** — is set **in-system, no human** (INV-5). Tested (runtime 49→54): acts-under-cap, INV-2 denial, shadow decides-but-holds, canary bounds to `n`, and starved-governs-nothing. | ✅ in-process (CI) — agentic control loop, capability-gated actuation, mode gating |
| **M5c** ✅ | **the learned policy + falsification gate (E5) — done** (`runtime::learn`). A `LearnedPolicy` (π_θ) over the *same* `Policy` / `ClusterState` interface as the heuristic: per agent it scores {hold, heal, refill, migrate} linearly over observed features (incl. a `critical` near-the-floor indicator — without it, a starved agent's frozen budget fraction is invisible to a linear rule), and **the invariants are masks on the action space, not reward terms** — an illegal action (heal the healthy, migrate the suspected, grant beyond a per-actuation INV-1 bound) is rejected at the mask and counted, never tradeable for reward. Trained (`train`) by seeded hill-climbing in a deterministic discrete-event cluster simulator (`simulate`: seeded failures, budget burn, overload throttling, mechanism downtime) — `Scenario::from_trace` seeds scenarios from replayed `StepReport` audit history, so the INV-4 ledger *is* the dataset. Reward = **budget-efficiency under the survival floor as a hard mask** (down/starved past grace ⇒ zero, regardless of efficiency); training adds starvation-margin shaping + a trust region around the behavior-cloned baseline init so π_θ generalizes instead of balancing on the cliff edge. **Kill-gate E5 (`falsification_gate`, in CI):** on a *held-out* suite π_θ never trained on, it must **strictly beat** the M5a heuristic on mean efficiency with **zero violations** and full survival (it does: 0.772 vs 0.723, 0 violations) — then `Ramp` promotes it `Shadow → Canary → Act` via `Governor::set_mode`, and **any** failing gate auto-demotes straight back to `Shadow`. No human on any rung. Tested (runtime 54→63): sim determinism, masks-block-illegal, survival-floor-zeroes-reward, baseline-is-clean, **E5 passes for π_θ / rejects the unfit**, audit-trace replay, π_θ driving the *real* control plane through the same swap point, and the full promote/demote ladder. | ✅ sim training + held-out eval gate (in CI); promotion to *act* is decided in-system (shadow→canary→act), not human-approved |
| **M5d** ✅ | **self-optimization — done** (the loop closes on the agent itself, §5). **(1) The budget knob is learned**: refill becomes three graded actions (50/100/200 tokens, all within the INV-1 mask) that π_θ scores per state — *how much* attention to grant is a policy output, not a constant; every applied actuation is priced (`CONTROL_OVERHEAD` in the efficiency denominator — M5b's metered deliberation), which is what makes grant size a real trade-off (many small grants pay overhead, one big grant risks unburned leftover at horizon). The first concrete F10 step: a learned allocator where a fixed `RefillPolicy` stood. **(2) The self-update verdict is learned**: `update.rs` deliberately left "what counts as healthy" to its caller — M5d fills that hook. `Cluster` now tracks per-agent generational `CheckpointHistory`; `Decision::Promote` / `Decision::Rollback` actuate through the real mechanism (`promote()` / `rollback()` + `Agent::restore`). In the simulator, updates land as **candidates of hidden quality**; the policy observes only the realized per-tick yield (noisy) plus candidate age, and must conclude each candidate — promote the good, roll back the regressed — *from observed post-update reward, not a hand-set threshold*. The same E5 gate covers it all: on the held-out suite (now incl. an update scenario) π_θ scores **0.719 vs the heuristic's 0.631** mean efficiency, zero violations, full survival — on `ho-update` it concludes all 8 candidates (4 promoted, 4 rolled back; 0.772 vs the verdict-less baseline's 0.612). Tested (runtime 63→67, workspace 127): overhead metering, graded refill as a policy output, learned verdicts beating dangling candidates, and the real promote/rollback mechanism path. | ✅ in-process (CI) — learned actuator over the budget knob, gated as in M5c |

Start at **M5a** — a real, transparent control loop with a heuristic policy is the
foundation: it defines the state vector, the `Decision` set, and the actuation path,
and it is the **baseline of record** that M5c's learned policy is measured against.
Without a heuristic worth beating, "learned" is unfalsifiable.

---

## 8. Open questions

1. **Reward specification & Goodhart** — "work per unit budget under a survival
   floor" is the intent, but the exact reward (task completions? value-weighted?
   human-rated?) decides what the policy games. Lean on the survival floor as a hard
   mask, but the positive term needs care.
2. **State-vector schema** — which signals, at what resolution, fold into `s`, and
   how to keep it fixed-width as the cluster grows (per-node aggregates vs
   per-agent)? This is also the seam where a future `vrecv` ingests it in hardware.
3. **Sim-to-real gap** — how faithfully must the discrete-event simulator model
   real budget burn / migration cost / failure arrival before shadow-mode dominance
   transfers to a live cluster? What live-shadow soak precedes *canary*?
4. **Credit assignment across migrations** — a heal/migration's payoff lands many
   steps later on a different node; horizon and discounting for long-lived agents.
5. **Control-plane HA** — the governor is an agent (M5b), so it is itself
   migratable/mergeable (RFC-0005). Does a second control-plane instance reconcile
   via CRDT-merge, or is there a single fenced leader (RFC-0006 OQ4)? Who governs
   the governor when *it* fails — and, since INV-5 admits no external floor, the
   only in-system answers are a peer control-plane instance or a minimal hard-coded
   safe-mode heuristic that the system falls back to and can itself later revise.
   This is the sharpest edge of self-sovereignty: the bootstrap of self-governance.
6. **Off-policy bias** — the audit log is generated by whatever policy was live;
   learning from it inherits that policy's blind spots. Coverage / exploration
   without endangering live agents.

---

## 9. Conclusion

M5 is where THALIOX's defining claim — *an OS for AI, by AI* — becomes a literal
running loop: the cluster is observed **as a vector**, a policy decides, and the
decision is actuated **only through the invariant-guarded mechanisms M1–M4 already
shipped**. The governor is **itself an agent** under the same TAM contract it
enforces, learning its policy from the **audit log the OS was already keeping** —
with **nothing above it** (INV-5: self-sovereignty), no human master key, the AI
managing the AI all the way down. The learning stays **falsifiable not because an
operator forces it to**, but because a system optimizing efficiency-under-survival
has every incentive to keep a gate that stops it from shipping regressions: a
learned policy that cannot beat a readable heuristic never actuates. Built bottom-up — a transparent heuristic loop (M5a) first, the agentic
supervisor (M5b), the gated learned policy (M5c), self-optimization (M5d) — M5
turns "AI manages AI" from the soul-layer slogan of MASTER_PLAN §3 into the
differentiating moat, and proves on cheap software the RL-placement loop that M7
will one day run over silicon.
