# THALIOX — an operating system for AI, by AI

> **"Let AI redefine AI" — An operating system for AI, by AI, ultimately for Humans.**

THALIOX is not "Linux + agents" stitched together. It is an operating system **designed top-down,
natively for AI agents**: vectors replace files, token streams replace byte pipes, attention budgets
replace CPU time slices, capability tokens replace uid/gid.

This repository is the **THALIOX core, rebuilt from scratch**, with the **TAM Abstract Machine contract**
([RFC-0001](docs/rfcs/0001-abstract-machine.md)) as its spine — so the software implementation (H1, validated
first on Linux) and future custom silicon (H3) obey **one shared semantics**. The evolution in between is
"swap the implementation," not "tear it down and start over."

## Three immovable principles

1. **Top-Down** — first define how application-layer agents work and what problems they solve, then make the runtime / kernel / hardware serve them, layer by layer. Hardware is the servant of the agent world, not a starting cage.
2. **Staged Moonshot** — every milestone is independently valuable, demonstrable, fundable, and falsifies the next stage.
3. **Humans are the Floor** — auditable, one-key takeover, reversible; never bypassable (INV-5 Sovereign capability).
4. **Clean-Slate Mandate** — not framed by human legacy (x86 / current CPUs·GPUs / the Linux kernel). **The purpose is efficiency** — to make the AI OS run faster and serve AI fully, not change for change's sake.

## TAM: three primitives · five invariants

Three first-principles primitives (see [RFC-0001](docs/rfcs/0001-abstract-machine.md)):

- **Vector Message** — the unit agents use to exchange *meaning*, not byte streams.
- **Attention Budget** — the unit of scheduling and accounting (tokens), replacing the CPU time slice.
- **Capability Token** — the unit of permission and trust, replacing uid/gid.

Five invariants constrain any implementation: **INV-1 budget conservation · INV-2 capability first (scope must be enforced) · INV-3 vector fidelity · INV-4 auditable · INV-5 humans are the floor**.

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

## Status: ✅ M1 single-node MVP shipped (2026-06-05, `v0.1.0`)

M1 proves the **programming model holds**: a single-node agent that, under the five TAM invariants,
completes a task autonomously. See [docs/M1-MILESTONE.md](docs/M1-MILESTONE.md). Delivered:

- **Cognition** — a unified `LlmProvider` wired to Anthropic Messages / OpenAI Chat Completions (and any compatible gateway), with an offline mock fallback.
- **Memory** — `SemanticSpace` vector memory (remember / recall).
- **Tools + autonomous loop** — `Agent::run(goal)`: **the model decides which tool to call** (`web_search` / `fetch`), executes it, feeds the result back, and keeps thinking until it answers.
- **Attention budget** — reservation → real-token reconciliation (INV-1 conservation), refunds on failure.
- **Capability gating** — every act verifies signature + expiry + scope (INV-2 first), fully audited (INV-4).
- **API gateway** — axum HTTP: `/agents` lifecycle + think / remember / recall / invoke + audit queries.

**Verified (glm-5.1 + Tavily)**: given a goal, the model autonomously calls `web_search` → real search → one-line summary;
audit trail `Think → ToolInvoke → Think`, budget reconciled line by line.

All four gates green: `fmt` · `clippy -D warnings` · `test` (30) · `doc -D warnings`.

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

H1 software (on Linux) → H2 specialization (push down the stack) → H3 co-designed silicon. Next up is
**M2 microVM packaging** (one-command deploy + snapshot/restore + self-update rollback). Full roadmap in
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
