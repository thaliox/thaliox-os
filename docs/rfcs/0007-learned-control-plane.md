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
> the heuristic before it is ever allowed to act. The invariants — and the human
> floor (INV-5) above all — are **hard constraints the learner cannot touch**.
> Delivers F10's first increment and de-risks F13. → the differentiating moat.

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

**And above it, unconditionally, sits the human (INV-5).** The Sovereign Capability
is held only by the human supervision plane and **cannot be delegated to the
control plane**. The learned policy can recommend suspend / snapshot / rollback /
terminate; it can never *be* the Sovereign. Whatever the control plane decides, the
human floor can override, freeze, or reverse it. M5 raises the ceiling of autonomy;
it does not lower the floor.

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
  **with zero invariant violations**. Only then is it promoted from *shadow* →
  *propose* (human-approved actuation) → *act*. A regression demotes it back to
  shadow automatically. **The invariants (INV-1/2/4) and the floor (INV-5) are hard
  constraints the optimizer cannot trade away** — they are masks on the action
  space, not terms in the reward.

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
| **Sovereign Capability** (INV-5) | the human floor **above** the control plane; never delegated to it |
| Mechanism vs policy (TAM §4.2) | M1–M4 = mechanism; **M5 = the policy** filling every deferred hook |
| Supervisor / migrate / merge (RFC-0005) | the **actuators** the control plane drives; M5 supplies the *when* |
| RL placement (MASTER_PLAN F13) | M5 places agents-on-nodes; **the same machinery places primitives-on-silicon in M7** |

---

## 7. Staged plan

| Stage | Deliverable | CI-gated? |
|---|---|---|
| **M5a** | **the control loop, heuristic baseline.** A `runtime::control` `ControlPlane` that folds cluster signals into a fixed-width state vector, runs a transparent hand-written `Policy` (`π(s) → Decision`), and actuates **only** through existing mechanisms (`Supervisor::self_heal` / `Node::migrate` / `place_remote` / budget refill). The `Policy` trait is the single swap point. Establishes the baseline every later policy must beat. | ✅ pure software (in CI) — loop + heuristic + actuation, on simulated cluster state |
| **M5b** | **the supervisor as an agent.** The `ControlPlane` becomes a first-class `Agent`: it *thinks* over the state vector (cognition loop), *spends* budget (INV-1) to deliberate, *acts under capability* (INV-2) on each actuation, and is *audited* (INV-4). Shadow / propose / act actuation modes; **INV-5 Sovereign stays with the human and is non-delegable.** | ✅ in-process (CI) — agentic control loop, capability-gated actuation, mode gating |
| **M5c** | **the learned policy + falsification gate (E5).** A learned `π_θ` trained off-policy on replayed audit traces in a discrete-event cluster simulator; reward = budget-efficiency under a survival floor; invariants are action-space masks, not reward terms. **Kill-gate:** `π_θ` runs in shadow and may not actuate until it strictly dominates the M5a baseline on a held-out scenario suite with zero invariant violations; regression auto-demotes to shadow. | ✅ sim training + held-out eval gate (in CI); promotion to *act* is human-approved |
| **M5d** | **self-optimization.** The control plane tunes each agent's `AttentionBudget` / adaptive-compute depth (RFC-0002) and *when* to roll/rollback a generational self-update (`update.rs`) as learned policies. The first concrete step of F10 — a learned budget allocator replacing a fixed `RefillPolicy`. | ✅ in-process (CI) — learned actuator over the budget knob, gated as in M5c |

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
   transfers to a live cluster? What live-shadow soak precedes *propose*?
4. **Credit assignment across migrations** — a heal/migration's payoff lands many
   steps later on a different node; horizon and discounting for long-lived agents.
5. **Control-plane HA** — the governor is an agent (M5b), so it is itself
   migratable/mergeable (RFC-0005). Does a second control-plane instance reconcile
   via CRDT-merge, or is there a single fenced leader (RFC-0006 OQ4)? Who governs
   the governor when *it* fails — the human floor, or a minimal hard-coded
   safe-mode heuristic?
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
and pinned, unconditionally, beneath the **human floor (INV-5)**. The learning is
**falsifiable**: a learned policy that cannot beat a readable heuristic never
actuates. Built bottom-up — a transparent heuristic loop (M5a) first, the agentic
supervisor (M5b), the gated learned policy (M5c), self-optimization (M5d) — M5
turns "AI manages AI" from the soul-layer slogan of MASTER_PLAN §3 into the
differentiating moat, and proves on cheap software the RL-placement loop that M7
will one day run over silicon.
