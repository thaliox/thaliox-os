# RFC-0009 — Agent learning & self-evolution: M9

| | |
|---|---|
| **Status** | Draft |
| **Author** | THALIOX core |
| **Supersedes** | — |
| **Depends on** | [RFC-0001 (TAM contract, operations, INV-1..5)](0001-abstract-machine.md), [RFC-0005 (CRDT merge, self-healing — the thing evolution is *not*)](0005-multi-instance-ha.md), [RFC-0006 (fabric, teams, gateway admission)](0006-cluster-multiplatform.md), [RFC-0007 (control plane; the E5 falsification discipline)](0007-learned-control-plane.md), [MASTER_PLAN §4 (F5, F9), §6 (M-ladder)](../MASTER_PLAN.md) |

> **This RFC designs M9 — two capability pillars that turn the agent from a
> managed unit of work into a subject that betters itself: LEARNING (distill
> skills from its own experience, adopt them, and diffuse them to peers) and
> SELF-EVOLUTION (sense its own inadequacy and change its mind, body, or trust
> domain for the better — including across an air gap).** It is the dual of
> M5: the control plane governs top-down; M9 lets agents negotiate upward and
> improve sideways. Together they are the full shape of "AI manages AI" — a
> society, not a command economy. Healing (M3/M5) restores an agent to what it
> was; **evolution makes it something better** — that distinction is the spine
> of this document. M9 is software-layer (L4), runs **parallel to M6**, and
> adds **zero new TAM operations**: every new behavior composes existing
> primitives, which is the strongest evidence yet that RFC-0001's abstraction
> was drawn in the right place.

---

## 1. Motivation

An M1–M5 agent is born with a fixed endowment: the mind it was constructed
with, the tools it was handed, no notion of a skill. Its lifecycle mechanisms
are conservative by design — heal, merge, migrate, roll back — every one of
them restores or relocates *the agent as it already is*. Nothing in the system
makes an agent **better** over its lifetime, and nothing lets it act on the
observation "this task is beyond my current substrate."

Three gaps, named precisely:

1. **Experience evaporates.** Every solved problem leaves a full INV-4 trace —
   what was tried, what failed, what worked, what it cost — and M5c already
   proved that this ledger is a training set (the governor learns from it).
   But the *agent that produced the trace* learns nothing; the next task
   starts from zero. The ledger-as-dataset move has been made once, at the
   control plane; M9 makes it again, at the agent.
2. **Competence is trapped.** When one agent works out how to solve a class of
   problem, the only way a peer benefits today is CRDT merge (full state
   union, M3) — an HA mechanism, not a teaching one. There is no object that
   carries "how I solved this" across the fabric.
3. **The substrate binding is static and top-down.** The governor places,
   refills, migrates (M5); `ResourceKind::Model` has made "which mind" a
   capability-addressable resource since day one — yet no mechanism exists for
   an agent to *request* a stronger mind or a stronger host because the work
   demands it, or to be sealed up and carried to a network it cannot reach.

Traditional agent frameworks cannot close these gaps: without metered budgets
there is no currency to price an upgrade, without capability tokens there is
no safe boundary between teaching and privilege, without migration there is no
body to move. THALIOX has all three primitives in production. **The features
that the incumbents have, we have; the features they cannot have, we build —
that is the innovation claim of M9, and it is a falsifiable one** (§5).

---

## 2. Pillar 1 — Learning (M9a distillation, M9b diffusion)

### 2.1 The Skill object

A **Skill** is a first-class memory object: a distilled, reusable strategy —
the *generalized residue* of solved tasks. It is not a weight update and not a
capability; it is knowledge, addressed and carried like any other semantic
object:

- **Distill** — the agent summarizes its own successful (and failed — negative
  knowledge is knowledge) INV-4 traces into a strategy: preconditions, the
  operation/tool sequence that worked, observed cost, failure modes. This is
  `MemSummarize` applied to the agent's audit history — the third reuse of
  "the ledger is the dataset" (M5c trained the governor on it; M9a trains the
  agent's *playbook* on it).
- **Store / recall** — skills live in the semantic space (`MemWrite`), are
  found by task similarity (`MemSearch`), and carry provenance: which agent
  distilled them, from which traces, with what measured outcome.
- **Apply** — at `Think` time, recalled skills are assembled into the
  reasoning context (the same slot M1's recall already fills). A skill makes
  the agent *smarter about how to act*; it grants nothing.

### 2.2 Diffusion

Skills move the way anything moves in THALIOX: as messages.

- **Push** — an agent that has just distilled a high-value skill (measured
  outcome above its own baseline) sends it to peers (`VSend` over the M4
  fabric); the M4c **Market/Swarm topologies are the diffusion graph** —
  market teams trade skills where demand is, swarm teams gossip them.
- **Pull** — an agent facing an unfamiliar task searches the shared space
  (RFC-0006 team spaces) before burning budget on rediscovery.
- **Gated adoption** — receiving a skill is not believing it. The adopter
  probes it (cheap trial against its own current playbook) and adopts only on
  measured improvement; adoption, rejection, and provenance are all audited
  (INV-4). Diffusion without gates is how a bad meme colonizes a society;
  diffusion with falsification gates is how a society learns.

### 2.3 The iron rule: skill ≠ capability

**Skills carry know-how. They never carry authority.** A skill that references
operations or tools its adopter holds no token for simply fails at the same
INV-2 door every act fails at — knowing *how* is not being *allowed to*.
Authority moves only along `CapDelegate`, only when the token is `delegable`,
exactly as before. Learning adds **zero** new paths through the capability
system, and E11 verifies that claim mechanically (§5). This line is what makes
free diffusion safe to want.

---

## 3. Pillar 2 — Self-evolution (M9c substrate seeking, M9d portable bundles)

### 3.1 Evolution is not healing

| | **Self-heal / self-update (F3, M3, M5d)** | **Self-evolve (M9)** |
|---|---|---|
| Trigger | failure, crash, divergence | *inadequacy* — the work exceeds the current self |
| Outcome | the same agent, restored / rolled back | a **better** agent: new skills, stronger mind, fitter body, new trust domain |
| Initiator | supervisor / control plane (top-down) | the agent itself (bottom-up), arbitrated by the control plane |
| Analogy | homeostasis | growth |

Healing keeps the society alive; evolution is why it is worth keeping alive.

### 3.2 M9c — substrate seeking (a market, not a free-for-all)

An agent senses its own inadequacy from signals it already has: budget burn
rate vs progress, failure loops in its audit trail, self-evaluation during
`Think`. On that signal it places a **bid** — a `VSend` to the control plane
(plain `Communicate` permission; **no new operation**) requesting a substrate
change:

- **a stronger mind** — rebinding `mind` to a better provider/model. The
  grant is a capability event: a new token scoped `{Model, pattern}`
  (`ResourceKind::Model` has been waiting for this since RFC-0001), and the
  rebind is possible precisely because the environment was excluded from
  `AgentState` by design;
- **a fitter body** — migration to a stronger node via the existing M4
  mechanism (`Node::migrate` / `place_remote`); nothing new moves the agent,
  only something new *asks* for the move;
- **more attention** — the M5d graded refill, which this RFC reframes as what
  it already was: the first substrate-seeking knob, dimension one of three.

The governor arbitrates bids exactly as it does everything else (RFC-0007):
as a policy decision over the cluster state, actuated only through M1–M4
mechanisms, audited, human-free (INV-5). **INV-1 is the market**: stronger
substrates carry higher attention prices, paid from the bidder's own budget —
an agent that escalates frivolously starves; an agent that never escalates
fails expensive tasks. Both failure modes are priced, so the equilibrium is
learned, not legislated (the bid-arbitration policy is one more `Policy` at
the M5 swap point, trainable by M5c machinery against the same simulator).

**Evolution runs downhill too**: choosing a *cheaper* mind for easy work is
the same decision with the same machinery, and E12's efficiency metric rewards
it. Seeking the fittest substrate, not the largest, is the point.

### 3.3 M9d — sealed portable bundles (evolution across the air gap)

Checkpoint/restore (M2) and migration (M4) assume a connected fabric. M9d
makes the agent **a thing you can carry**: 

- **Export** — a sealed, content-addressed bundle: `AgentState` (budget, caps,
  phase, audit — the portable core that already exists), a memory shard (the
  agent's owned slice of the semantic space), its skill set (§2), a tool
  manifest, and optionally a local model (F5) so the bundle thinks offline.
  Signed, hash-addressed, verifiable with no network. Composes `Checkpoint`
  (`Admin`-gated, existing) with a memory export; **no new TAM operation**.
- **Import = re-admission, not restore.** Capability tokens are signed against
  a cluster's root; a different cluster is a different trust universe. The
  receiving gateway (M4d — already the capability-admission front door)
  verifies the bundle's integrity and provenance, then **re-issues tokens
  under its own root according to its own policy**. The old tokens are not
  honored — they are a *verifiable résumé* of what the agent was once trusted
  to do, evidence for the admission decision, nothing more. E13 verifies that
  an imported agent's old-root tokens are dead on arrival (§5).
- This is the deployment story for disconnected, embargoed, or hostile-network
  environments: distill, seal, carry, re-admit — the agent arrives with its
  competence and none of its old authority.

Identity continuity is deliberate: the bundle carries the `AgentId` and the
full audit chain, so "the same agent, in a new trust domain" is a checkable
claim, not a metaphor.

---

## 4. New surface: none

The entire RFC composes existing primitives:

| New behavior | Composed from |
|---|---|
| distill / store / recall / apply skill | `MemSummarize` / `MemWrite` / `MemSearch` / `Think` |
| diffuse / probe / adopt skill | `VSend` + team topologies (M4c) + gated trial + INV-4 |
| escalation bid | `VSend` to the governor (`Communicate`) |
| mind rebind | capability grant `{Model, …}` + environment reattach (RFC-0002 §3.4) |
| body change | `Node::migrate` (M4), unchanged |
| export / import | `Checkpoint`/`Restore` (`Admin`) + memory export + M4d gateway admission |

`Operation`, `Permission`, and INV-1..5 are untouched. RFC-0001 enters its
seventh milestone unchanged — the contract keeps being the right one.

---

## 5. Staged plan

Gate numbering continues from M6 (E6–E9): **E10–E13**. All four stages are
CI-able — no hardware, no spend; the deterministic simulator (M5c) and
in-process multi-cluster harnesses carry every gate.

| Stage | Deliverable | Gate (falsification) | Where it runs |
|---|---|---|---|
| **M9a** | **skill distillation**: Skill object + distill-from-audit + recall-and-apply at `Think` time | **E10**: on a held-out task suite, the agent with its distilled playbook strictly beats its skill-less clone (same mind, same budget, same tools) on success rate *and* budget-per-success; zero invariant violations | CI (simulator + replayed traces) |
| **M9b** | **diffusion**: push/pull over fabric + gated adoption + provenance audit | **E11**: a skill distilled by one agent measurably lifts naive peers post-adoption (vs their pre-adoption baseline); a deliberately poisoned skill is rejected by the adoption gate; the INV-4 ledger shows **zero** authority movement outside `CapDelegate` — mechanically checked | CI (multi-agent in-process cluster) |
| **M9c** | **substrate seeking**: inadequacy signals + bid protocol + governor arbitration policy + mind-rebind grant path | **E12**: on a held-out mixed-difficulty suite, the bidding policy strictly beats *both* trivial baselines — *always-strongest* (on total budget spent) and *never-escalate* (on success rate) — i.e. higher reward-per-budget than either; zero violations; bids from starved agents cannot overdraw (INV-1 mask) | CI (simulator; model tiers as priced mock providers) |
| **M9d** | **sealed bundles**: export/import + gateway re-admission ceremony | **E13**: round-trip an agent between two clusters with **distinct capability roots** and no shared fabric (export → offline verify → import); conformance suite green on arrival; skills and audit chain intact; every old-root token provably refused in the new cluster | CI (two in-process clusters, distinct roots); later optional: netns-isolated bare-metal demo |

Order: M9a → M9b (diffusion needs something to diffuse); M9c and M9d are
independent of each other and of M9b, and can land in any order after M9a.
Gate factors are provisional until each stage's baseline harness freezes them,
the E1–E9 way. **M9 shares no resources with M6** — it touches no substrate
code — so the two milestones proceed in parallel by construction.

---

## 6. Mapping to TAM & the master plan

| Concept | M9 realization |
|---|---|
| **F14 agent learning / F15 self-evolution & portability** | minted by this RFC (MASTER_PLAN §4): pillar 1 *is* F14, pillar 2 *is* F15 |
| **F9 digital biology** | first concrete leg: skill adoption is heritable *mutation*, adoption/arbitration gates are *selection*, diffusion is the population mixing — evolution of fitter individuals at runtime, exactly as written |
| **F5 offline local model** | rides in the bundle: the exported agent thinks where there is no network |
| **INV-1 attention economy** | promoted from meter to **market**: substrate prices make self-evolution an economic decision, and starvation — not regulation — bounds ambition |
| **INV-2 capability-first** | the skill/capability split (§2.3): know-how diffuses freely, authority moves only along delegation; import re-roots trust instead of honoring foreign tokens |
| **INV-4 auditable** | the ledger is the dataset for the *third* time (E5: governor; M9a: playbook); skills carry trace-level provenance; adoption decisions are themselves audited |
| **INV-5 self-sovereignty** | no human approves an evolution step: bids, arbitration, adoption, re-admission are all in-system policies |
| Mechanism/policy split (TAM §4.2) | *when to escalate / what to adopt / whom to admit* are swappable policies at the M5 swap point — trainable, falsifiable, demotable |
| M5 (RFC-0007) | M9 is its dual: top-down governance meets bottom-up negotiation; the governor gains a bid stream as input and an arbitration head as output |
| MELD (RFC-0003) | skills are the software rehearsal of mergeable cognition: when minds become mergeable (M7/M8 silicon), "adopt a skill" compiles down to a state merge — the M9 interfaces are written to survive that swap |

---

## 7. Open questions

1. **Skill representation** — v1 is distilled text/structured strategy (cheap,
   inspectable, provider-portable). When local models (F5) mature, do skills
   graduate to adapter-grade artifacts (LoRA-style), and does the adoption
   gate's probe-trial survive that jump in opacity?
2. **Memetic safety** — the adoption gate stops measurably-bad skills; what
   stops a skill that probes well but degrades rarely-exercised behavior?
   Is the answer periodic re-validation (skills decay unless re-proven), and
   does that pressure belong in the same E10 harness?
3. **Market fairness vs selection** — budget-priced escalation means poor
   agents evolve less. Within F9 that is selection working; at what point does
   it become a monoculture risk (the rich get smarter), and is the governor's
   arbitration policy the right place for a diversity term?
4. **Identity across re-admission** — the bundle carries `AgentId` + audit
   chain, but the new cluster re-issues all authority. Is "the same agent"
   a claim about state continuity (our answer today) or about authority
   continuity (explicitly rejected) — and does any protocol need the
   distinction surfaced?
5. **Bid-channel pressure** — inadequacy signals derive from the agent's own
   audit; a policy could learn to over-report to farm refills. INV-1 pricing
   bounds the damage, but should the arbitration policy cross-check bids
   against the substrate ledger (M6a) once it exists?

---

## 8. Conclusion

M9 closes the loop that M1–M5 left open: the society could run, heal, migrate,
cluster, and govern itself — but no individual in it could *grow*. After M9,
an agent distills what its experience taught it, teaches its peers under a
gate that keeps teaching honest, buys itself a better mind or a better body
when the work demands one and pays for it from its own attention, and can be
sealed, carried across an air gap, and re-admitted into a foreign trust domain
with its competence intact and its old authority deliberately void. Every
claim is gated (E10–E13) against baselines a skeptic can re-run; every new
behavior is a composition of primitives that have already survived eight
RFCs. Healing kept the society alive. Evolution is what it is alive *for* —
and it arrives, like everything in THALIOX, as a falsifiable curve rather than
a slogan.
