# RFC-0005 — Multi-instance HA: migration, CRDT merge, self-healing

| | |
|---|---|
| **Status** | Draft |
| **Author** | THALIOX core |
| **Supersedes** | — |
| **Depends on** | [RFC-0001 (TAM §6)](0001-abstract-machine.md), [RFC-0002 (Model-State Contract)](0002-near-term-model-architecture.md), [RFC-0003 (MELD pillar 2)](0003-meld-cognitive-substrate.md), [RFC-0004 (Firecracker)](0004-firecracker-deploy.md), [MASTER_PLAN.md](../MASTER_PLAN.md) |

> **This RFC designs M3 — making a running agent survive node loss.** It cashes in
> the three operations [RFC-0001 §6](0001-abstract-machine.md) already promised on
> the `Checkpoint`: **migration** = rebuild from a checkpoint on the target node;
> **merge** = combine two diverged checkpoints via CRDT; **self-healing** = restore
> a faulty instance's last good checkpoint on a healthy one.
> M3 is not a from-scratch build — it composes M2's primitives (`Checkpoint`,
> `CheckpointHistory`, the F2b/F3 vsock control channel) and finally implements the
> `merge` that RFC-0002 §4 C-4 left as a stub.

---

## 1. Motivation

M2 made a running agent an OS-managed object: snapshottable, deployable, rollback-able.
M3 makes it **survivable** — an agent (and its in-flight state) outlives the node it
runs on. That is the F4 capability and the precondition for any cluster (M4+): you
cannot orchestrate a fleet you cannot keep alive.

The pieces already exist, scattered: a portable `Checkpoint` (M2), a way to *pull* one
from a live microVM over vsock (F3 `MicroVm::checkpoint`), and a generational history
(`CheckpointHistory`, M2). M3 wires them into three flows and supplies the one missing
primitive — a real `CognitiveState::merge`.

---

## 2. Cluster model (minimal)

```
   ┌── Node A ──┐         ┌── Node B ──┐
   │ agent a1   │  vsock  │ (standby)  │
   │ MicroVm    │ ──────▶ │            │
   └────────────┘  ckpt   └────────────┘
        │ heartbeat ▲           ▲
        └───────────┴── Supervisor ──┘
                     (registry + health)
```

- **Node** — a host that can run agents (an in-process `LocalDeploy` host, or a
  Firecracker host from RFC-0004). Identified by a `NodeId`.
- **Supervisor** — holds a registry `AgentId → (NodeId, last Checkpoint, health)` and a
  health signal per agent. *Mechanism, not policy* (TAM §4.2): it exposes the failure
  signal and the migrate/heal operations; *when* to act is a pluggable policy (and the
  M5 learned control plane later).
- No new wire protocol: control reuses RFC-0004 `vmproto` (health / checkpoint) over
  vsock; checkpoint transfer between nodes is the `Package` bytes (RFC-0002).

---

## 3. Live migration

Move agent `a1` from node A to node B with minimal interruption:

1. **Capture** — pull a fresh `Checkpoint`/`Package` from a1 (in-process, or over vsock
   `MicroVm::checkpoint`).
2. **Transfer** — ship the `Package` bytes to B.
3. **Restore** — `Agent::restore` / `FirecrackerDeploy` on B; rebind the environment
   (memory/mind are external & addressable, RFC-0002 §3.4).
4. **Cutover** — B becomes authoritative; A is drained/stopped; the supervisor registry
   flips `a1 → B`.

The primitives for 1–3 already exist. The "live" question is **downtime**:
- **M3 baseline = stop-and-copy** — pause, checkpoint, transfer, restore, resume. Brief
  pause; simplest; correct. The bounded-state model (RFC-0002) makes the checkpoint
  small, so the pause is short.
- **Future = pre-copy** — checkpoint while running, ship the delta, then a tiny final
  stop-and-copy. Deferred; needs incremental checkpoints.

---

## 4. CRDT merge — implementing `CognitiveState::merge`

Two instances of an agent can diverge (a fork that rejoins, or a healed split-brain
where both ran briefly). Merge must be **conflict-free and lawful** — the RFC-0003
pillar-2 property, whose toy gate **E1 passed**. We give `AgentState`
(`{budget, caps, phase, audit}`) a real merge by composing a CRDT per field:

| Field | CRDT | Merge rule |
|---|---|---|
| `audit` | grow-only set / append log | union by record identity `(agent, op, at, target)`, sorted by `at` — no record is ever lost |
| `caps` | G-Set | union by `jti` (dedup) |
| `budget.spent` | join (max) | `max(spent_a, spent_b)` — monotone, never *under*-counts spend (conservative & idempotent); `total`/`rate`/`refill` are config and must match |
| `phase` | join over a precedence lattice | a fixed order so merge is commutative/associative/idempotent; policy choice documented (e.g. a terminal `Dead` absorbs; otherwise the more-active state) |

Each field rule is **commutative, associative, idempotent** ⇒ the whole is a lawful
CRDT, exactly the laws **E1 validated** at toy scale. The merge tests will assert those
laws (as E1 does) and that no audit record is lost. This closes the RFC-0002 §4 C-4 stub
and answers **RFC-0001 Open Question #3** for the agent layer (a *structured*, per-field
CRDT, not a blind blob merge).

> Honest scope: this merges the **agent runtime state**. Merging a future *model's*
> latent state is the harder MELD pillar-2 problem (RFC-0003) — out of M3 scope.

---

## 5. Self-healing takeover

1. **Detect** — the supervisor heartbeats each agent (vsock `health`); N missed beats ⇒
   suspected-down.
2. **Confirm & fence** — avoid split-brain: mark the old instance dead/fenced before
   takeover (and, if it returns, reconcile via §4 merge rather than running two).
3. **Restore** — bring the agent up on a healthy node from its **last committed**
   `CheckpointHistory` generation (M2) — the `restore` of RFC-0001 §6.
4. **Resume** — registry flips to the new node; service continues.

This is migration (§3) triggered by a failure signal instead of an operator, plus the
fencing/merge that keeps it safe.

---

## 6. Mapping to TAM (RFC-0001 §6)

| TAM §6 promise | M3 realization |
|---|---|
| **Migration** = rebuild from the Checkpoint on the target node | §3 capture → transfer → restore |
| **Merge** = two Checkpoints merged without conflict via CRDT | §4 `CognitiveState::merge` (per-field CRDT) |
| **Self-healing** = last Checkpoint of a faulty instance restored on a healthy one | §5 heartbeat → fence → restore from `CheckpointHistory` |

M3 is where TAM §6 stops being a promise and becomes code.

---

## 7. Staged plan

| Stage | Deliverable | CI-gated? |
|---|---|---|
| **M3a** ✅ | real `CognitiveState::merge` for `AgentState` (per-field CRDT) + `Checkpoint::merge` + law/no-loss tests. **Done** — `crates/runtime/src/agent.rs`. | ✅ pure software (in CI) |
| **M3b** ✅ (in-process) | migration flow: capture → transfer → restore. **Done in-process** — `runtime::cluster` `Node` + `migrate` (stop-and-copy via the `Package` bytes), tests prove state survives + cutover + reversibility. Cross-Firecracker-host migration reuses the same flow over F3's vsock and is deferred to a KVM host. | ✅ in-process (in CI); self-hosted for VM |
| **M3c** ✅ | supervisor: registry + heartbeat + fenced self-healing takeover. **Done** — `runtime::supervisor` (`observe`/`tick`/`health`/`self_heal`/`reconcile`): detect → restore last good on a healthy node → flip registry; a returning split-brain is reconciled via the M3a CRDT merge. | ✅ in-process (in CI) |
| **M3d** | (optional) pre-copy live migration to shrink downtime | later |

Start at **M3a**: it is self-contained, CI-testable, closes the merge stub, and gives §5
its conflict-resolution backbone.

---

## 8. Open questions

1. Fencing mechanism — a supervisor lease/epoch, or a capability the supervisor revokes
   on the old instance (INV-2)? The capability route keeps it inside TAM.
2. `phase` merge precedence — what exactly absorbs what (is `Dead` terminal under merge,
   or can a heal resurrect)?
3. `budget.spent` under independent runs — is `max` enough, or do we need a per-replica
   PN-counter to sum genuinely-disjoint spend without double-counting?
4. Heartbeat transport at cluster scale — vsock per host is fine; cross-host needs the
   fabric / vector-transport layer (M4 / H3).
5. Migration consistency for external memory — the `SemanticSpace` is shared/addressable;
   does a migrating agent need a memory barrier, or is eventual consistency acceptable?

---

## 9. Conclusion

M3 makes agents survivable by composing what M2 already built and adding one real
primitive — a **per-field CRDT merge** whose laws E1 already validated. Migration,
merge, and self-healing are the three operations RFC-0001 §6 promised on the Checkpoint;
M3 turns them into code, starting with the merge (M3a) that everything else leans on.
