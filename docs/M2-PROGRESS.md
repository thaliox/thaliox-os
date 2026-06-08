# M2 — microVM-ization progress note

> **Status: 🚧 In progress · software layer complete (all in the CI gate) · Firecracker deferred to a KVM host**
> M2's three deliverables — **one-click deployment + snapshot/restore + self-update rollback** ([MASTER_PLAN](MASTER_PLAN.md) §6, delivering F2/F3) — are **done in software**. The only remaining leg is booting the deployment unit inside a real microVM, which waits on a KVM-capable host (see §5).

M2 is the second rung of the H1 horizon. Its job is to make a running agent an **OS-managed object** — something the platform can snapshot, ship, deploy, and roll back — turning [RFC-0002](rfcs/0002-near-term-model-architecture.md)'s **Model-State Contract** from a paper §4 into runnable code, and giving the TAM `Checkpoint` (RFC-0001 §6) real `checkpoint` / `restore` semantics.

## 1. What was delivered

The software-first stance: model the microVM **boundary** in pure Rust now, behind interfaces a Firecracker target slots into later without reshaping the data.

| Capability | crate · module | Description |
|---|---|---|
| Model-State Contract | `cognition::state` | `CognitiveState`: `serialize` (TAM §6 Checkpoint) + `restore` (migration) + `merge` (defaults to `Unsupported`, RFC-0002 C-4 — the gap RFC-0003 pillar 2 closes) |
| Snapshot / restore | `runtime` (`Agent::checkpoint` / `restore`) | The **portable** state (budget + caps + phase + audit) serializes to a blob; the environment (memory / mind / tools / verifier) is rebound on restore, since long-term memory is external and addressable (RFC-0002 §3.4) |
| Self-update rollback | `runtime::update` | `CheckpointHistory` generations: `init` a committed baseline → `stage` a candidate → `promote` or `rollback`; `conclude_update` restores the agent from the last good generation on an unhealthy update |
| Packaging / one-click deploy | `runtime::package` | `Package = Manifest + Checkpoint`, serializable to one shippable byte artifact; `DeployTarget` launcher interface; `LocalDeploy` validates the manifest (model fingerprint, required tools, format) against the host-bound `DeployEnv` and restores in-process |

## 2. How it maps to TAM (RFC-0001 §6)

| TAM concept | M2 realization |
|---|---|
| **Checkpoint** | `Agent::checkpoint()` → `CognitiveState::serialize` of the portable state |
| **Migration** | `Agent::restore(checkpoint, memory, mind)` — rebuild on a fresh environment / node |
| **Merge** | `CognitiveState::merge` exists but returns `Unsupported` — deferred to M3 + RFC-0003 pillar 2 (E1-validated) |
| **Self-update / rollback** | generational `CheckpointHistory` + `conclude_update` |
| **Deployment unit** | `Package` byte artifact + `DeployTarget` (software `LocalDeploy` today) |

## 3. Quality gates

- **All software, no infra** — every M2 module runs in the existing pure-Rust CI gate (no KVM, no network).
- The M2 work adds **15 unit tests** (`cognition::state` 3; `runtime` checkpoint 2, `update` 4, `package` 6), including the checkpoint round-trip onto a fresh node, rollback-to-baseline, and the pack → bytes → deploy loop with rejection of missing-tool / model-mismatch / bad-format.
- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, and `cargo test` are green workspace-wide.

## 4. Empirical evidence

- **Snapshot/restore**: an agent that has spent budget, holds a capability, and has an audit trail is checkpointed, then `restore`d onto a brand-new memory + mind; its state survives and re-checkpointing yields a **bit-identical** blob.
- **Rollback**: after a "bad update" drains budget and adds audit, an unhealthy verdict restores the agent to the committed baseline — budget and audit back to the pre-update values, the candidate generation dropped.
- **One-click deploy**: a `Package` serialized to bytes is parsed on a fresh host and `LocalDeploy`-ed into a live agent with its state intact; a manifest demanding an unbound tool or a mismatched model is rejected before launch.

## 5. Deliberate gaps (the remaining leg)

- **Firecracker target** — `DeployTarget` has only `LocalDeploy` (in-process). A `FirecrackerDeploy` booting the package inside a microVM will implement the **same trait**; the package format and manifest validation are unchanged, so this lands without reshaping anything. It requires a **KVM-capable host** (`/dev/kvm`), an uncompressed guest kernel + rootfs, `jailer` for isolation, and a self-hosted CI runner — none of which the current pure-cargo gate provides. Wired when that host exists.
- **Cross-process snapshot** — today restore is in-process; the byte artifact is ready to cross a process / VM boundary, but that path is exercised only once a real target exists.
- **Merge is a stub** — `CognitiveState::merge` is `Unsupported` by design; real mergeable state is M3 territory (RFC-0003 pillar 2, gated by E1).

## 6. Next stop

- **M3 multi-instance HA** — live migration + CRDT merge + self-healing takeover, built directly on the `Checkpoint` this milestone made real.
- **Firecracker** — implement `FirecrackerDeploy` against `DeployTarget` once a KVM host is provisioned.
