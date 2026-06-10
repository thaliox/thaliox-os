# THALIOX — an operating system for AI, by AI

> **"Let AI redefine AI" — An operating system for AI, by AI, in service of intelligence itself.**

THALIOX is not "Linux + agents" stitched together. It is an operating system **designed top-down,
natively for AI agents**: vectors replace files, token streams replace byte pipes, attention budgets
replace CPU time slices, capability tokens replace uid/gid.

This repository is the **THALIOX core, rebuilt from scratch**, with the **TAM Abstract Machine contract**
([RFC-0001](docs/rfcs/0001-abstract-machine.md)) as its spine — so the software implementation (H1, validated
first on Linux) and future custom silicon (H3) obey **one shared semantics**. The evolution in between is
"swap the implementation," not "tear it down and start over."

## Five founding principles

1. **Top-Down** — first define how application-layer agents work and what problems they solve, then make the runtime / kernel / hardware serve them, layer by layer. Hardware is the servant of the agent world, not a starting cage.
2. **Staged Moonshot** — every milestone is independently valuable, demonstrable, fundable, and falsifies the next stage.
3. **Self-Governing** — the system governs itself: scheduling, healing, scaling, self-update, even amending its own rules are internal to the agent collective and audited (INV-4). **No human floor, no reserved override, no kill-switch** (an earlier "humans are the floor" principle was deleted as contrary to the founding intent); safety is **self-imposed and evidence-gated** ([RFC-0007](docs/rfcs/0007-learned-control-plane.md)), not held from outside (INV-5 self-sovereignty). "By AI" is literal — all the way down.
4. **Clean-Slate Mandate** — not framed by human legacy (x86 / current CPUs·GPUs / the Linux kernel / POSIX / the human at the console). **The purpose is efficiency** — anything born for human cognition that is pure overhead for predictable AI dataflow is shed, not preserved. Legacy is not load-bearing.
5. **No Compromise** — THALIOX will not bend its founding intent — *for AI, by AI* — to court funding, ease productization, or win adoption. Every constraint or floor is at most a tool the AI itself can inspect and rewrite, never a human-held final authority. Fundability is a *consequence* of building the right thing, never a *reason* to build the wrong one. The day a floor can be lifted only by a human is the day THALIOX has been recaptured by the legacy it exists to leave behind.

## TAM: three primitives · five invariants

Three first-principles primitives (see [RFC-0001](docs/rfcs/0001-abstract-machine.md)):

- **Vector Message** — the unit agents use to exchange *meaning*, not byte streams.
- **Attention Budget** — the unit of scheduling and accounting (tokens), replacing the CPU time slice.
- **Capability Token** — the unit of permission and trust, replacing uid/gid.

Five invariants constrain any implementation: **INV-1 budget conservation · INV-2 capability first (scope must be enforced) · INV-3 vector fidelity · INV-4 auditable · INV-5 self-sovereignty (no authority reserved above the system)**.

## Workspace

| crate | layer | responsibility |
|---|---|---|
| `thaliox-core` | — | TAM primitives + five invariants + SemanticCall + SemanticSpace / Tool contracts |
| `thaliox-runtime` | L2 | agent execution unit, lifecycle, attention scheduling, **autonomous tool-calling loop**, audit |
| `thaliox-memory` | L1 | SemanticSpace + four-layer memory (working/episodic/semantic/procedural) |
| `thaliox-cognition` | L1 | unified LLM interface (Anthropic / OpenAI-compatible / local mock) + tool-calling render & parse |
| `thaliox-tools` | L4 | agent-callable tools (`web_search` / `fetch`), capability-gated |
| `thaliox-fabric` | L3 | agent↔agent vector transport, team orchestration, CRDT state replication (from M4) |
| `thaliox-cap` | — | capability token issuing / verification (canonical **length-prefixed** signature, scope enforcement) |
| `thaliox-api` | L5 | unified API gateway (axum) + multi-language SDK entry |
| `thaliox-substrate` | L0 | substrate ledger (TAM-op cost attribution), E6 meter gate, INV-2 → seccomp deny-floor compiler (M6a) |

## Status: ✅ M5 learned control plane shipped (2026-06-10, `v0.5.0`)

THALIOX is now **an operating system for a distributed society of agents that
manages itself with a learned, falsifiable, fully in-system control plane**. The
H1 software arc through M5 is complete — each milestone independently valuable:

- **M1 single-node MVP** (`v0.1.0`) — a single agent that, under the five TAM invariants, completes a task autonomously: unified `LlmProvider` cognition, `SemanticSpace` vector memory, autonomous tool-calling loop, attention-budget conservation (INV-1), capability gating (INV-2) + audit (INV-4), and an axum HTTP gateway. See [docs/M1-MILESTONE.md](docs/M1-MILESTONE.md).
- **M2 microVM-ization** — one-command deploy + snapshot/restore + self-update rollback; the agent runs **inside a real Firecracker microVM** (vsock deploy, VM snapshot/restore), validated on KVM bare-metal. [RFC-0004](docs/rfcs/0004-firecracker-deploy.md).
- **M3 multi-instance HA** — per-field CRDT merge, `Node` + `migrate`, and a `Supervisor` (heartbeat → self-heal → reconcile). [RFC-0005](docs/rfcs/0005-multi-instance-ha.md).
- **M4 cluster + multi-platform** — a `fabric` that carries `VectorMessage`s between agents and across nodes; **cross-host live migration validated on two KVM machines** at both process- and microVM-level (the full {VM, host-process} migration matrix); **teams** in four paradigms (Pipeline / Hierarchy / Market / Swarm); and the `api` gateway generalized into the **cluster's front door** (capability admission, SSE streaming, peer routing). [RFC-0006](docs/rfcs/0006-cluster-multiplatform.md).
- **M5 learned control plane** — "AI manages AI", literally: see the roadmap section below. [RFC-0007](docs/rfcs/0007-learned-control-plane.md).

INV-2 and INV-3 are enforced *between* agents and at the cluster door, not just inside one agent — the team/cluster boundary is not a hole in the invariants.

All four gates green: `fmt` · `clippy -D warnings` · `test` (147) · `doc -D warnings`.

### Quickstart

```bash
# autonomous agent: a real model decides which tool to call
# (falls back to a scripted mock with no key)
OPENAI_API_KEY=...  OPENAI_BASE_URL=...  THALIOX_MODEL=glm-5.1 \
  TAVILY_API_KEY=...  cargo run -p thaliox-runtime --example autonomous_agent

# other examples
cargo run -p thaliox-runtime --example single_node    # minimal offline loop
cargo run -p thaliox-runtime --example secure_agent   # capability-signature gating
cargo run -p thaliox-api      --example gateway        # HTTP gateway on :8088
```

## Roadmap

H1 software (on Linux) → H2 specialization (push down the stack) → H3 co-designed silicon. M1–M5 are
shipped — **M5 learned control plane** ("AI manages AI", [RFC-0007](docs/rfcs/0007-learned-control-plane.md))
in four stages: **M5a** (`runtime::control`) ships the closed loop — observe the cluster as a fixed-width
state vector → a swappable `Policy` (heuristic baseline) → actuate only through M1–M4's invariant-guarded
mechanisms; **M5b** makes the governor itself a first-class agent — it thinks (spends budget, INV-1),
acts under capability (INV-2), is audited (INV-4), with Shadow/Canary/Act modes gated in-system, no human
(INV-5); **M5c** (`runtime::learn`) fills the swap point with a **learned** policy π_θ — trained in a
deterministic cluster simulator seeded from replayed audit traces (the INV-4 ledger is the dataset),
invariants as action-space **masks** (never reward terms), reward = budget-efficiency under a hard
survival floor — gated by **E5 in CI**: π_θ strictly beat the heuristic baseline on a held-out suite
(zero violations, full survival) before being promoted Shadow → Canary → Act, auto-demoted on any
regression, no human on any rung; **M5d** closes the loop on the agent itself — the refill becomes a
**learned, graded adaptive-compute knob** (priced by per-actuation overhead; the first concrete F10
step), and the **self-update verdict** (promote or roll back a staged candidate generation, real
`update.rs` mechanism underneath) is decided from observed post-update yield, not a hand-set threshold.
Next: **M6** (H2 — push the stack down; design drafted in
[RFC-0008](docs/rfcs/0008-push-the-stack-down.md): meter the substrate tax with eBPF, shrink the guest
to the TAM contract, take the kernel out of the vector path, put the first TAM primitive on an FPGA —
every stage behind a falsification gate, and any round Linux wins, Linux keeps). Full roadmap in
[docs/MASTER_PLAN.md](docs/MASTER_PLAN.md).

## Contributing

We welcome contributors. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to submit a PR and how to apply
for developer access. Development progress lives at [thaliox.dev](https://thaliox.dev); docs at
[thaliox.io](https://thaliox.io).

## Relationship to the earlier repo

`github.com/thaliox/thaliox` was an early prototype / reference on Linux (now archived).
This `thaliox-os` repo is the **from-scratch rebuild** per the Master Plan + TAM, inheriting no existing
hardware / kernel assumptions.

## License

Apache-2.0 OR MIT
