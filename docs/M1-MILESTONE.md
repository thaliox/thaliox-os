# M1 — single-node MVP milestone summary

> **Status: ✅ Done · `v0.1.0` · 2026-06-05**
> Hypothesis proven: **the THALIOX programming model holds** — a single-node agent, under the constraints of the TAM five invariants, can complete a task autonomously.

M1 is the first rung of the H1 horizon in [MASTER_PLAN](MASTER_PLAN.md) §6. Its sole job is to turn
[RFC-0001 TAM Abstract Machine](rfcs/0001-abstract-machine.md) from a paper contract into **runnable, falsifiable** code,
running on Linux first but with semantics not hard-wired to Linux — leaving the seams for H2/H3 "replace the implementation."

## 1. What was delivered

One end-to-end loop: **give the agent a goal → the model autonomously decides which tool to call → execute → feed the result back → think again → produce an answer**,
all of it bounded by the attention budget and capability tokens, audited per operation.

| Capability | crate | Description |
|---|---|---|
| Cognition | `cognition` | Unified `LlmProvider::complete(messages, tools)`; bidirectional render + parse for Anthropic Messages / OpenAI Chat Completions (and any compatible gateway); offline `MockProvider` fallback |
| Memory | `memory` | `SemanticSpace` vector memory, remember / recall |
| Tools | `tools` | `web_search` (Tavily) / `fetch`, implementing the `Tool` contract, with `description()` broadcast to the model |
| Autonomous loop | `runtime` | `Agent::run(goal, max_iters)`: think (broadcast tools) → model `tool_calls` → act (Invoke) → feed result back → think again; failures are fed back too so the model can self-correct |
| Budget | `core` / `runtime` | Reserve → real-token `settle` reconciliation; refund on failure |
| Capability | `core` / `cap` | Signature + expiry + scope triple check, enforced before act |
| Audit | `runtime` | Every think / invoke records op·cost·target·allowed |
| Gateway | `api` | axum HTTP: agent lifecycle + think / remember / recall / invoke + audit queries |

## 2. How the five invariants map to the implementation

| Invariant | How M1 enforces it |
|---|---|
| **INV-1 budget conservation** | Every think / invoke first deducts against a reservation, then `settle(reserved, actual)` reconciles to real tokens after execution; on failure `settle(reserved, 0)` refunds in full |
| **INV-2 capability first** | `act` verifies the capability token before any side effect: signature (pluggable `CapabilityVerifier`) + not expired + `authorizes(perm, resource, target)` scope enforcement |
| **INV-3 vector fidelity** | Memory is stored and retrieved via `SemanticSpace`, never downgraded to string keys |
| **INV-4 auditable** | Every `SemanticCall` writes an `AuditRecord` (op / cost / target / permission_used / allowed) |
| **INV-5 humans are the floor** | Capabilities are revocable, the budget has a hard ceiling, the full audit is replayable; the Sovereign overrides everything |

## 3. Empirical evidence

**Real model glm-5.1 (via an OpenAI-compatible gateway) + Tavily web_search**, goal:
> "Use web_search to find out what 'THALIOX AI-native operating system' is, then summarize what you saw in one sentence."

The model **autonomously** decided to call `web_search` (not orchestrated by the caller), ran a real search, and summarized after the result was fed back. Audit trail:

```
✓ Think       cost=315  self            ← model sees the tool description, decides to call web_search
✓ ToolInvoke  cost=303  tool://web_search ← real Tavily search
✓ Think       cost=858  self            ← summarizes from the search result
remaining 48524 / 50000 (per-operation real-token reconciliation)
```

This step is the qualitative leap from "orchestrated tool execution" to "autonomous agent": decision authority sits with the model, constraint authority with TAM.

## 4. Quality gates

All four gates green (CI iron law, see [rust-toolchain](../README.md)):

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` — **30 tests** (pure functions + loop + gateway oneshot)
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`

8 crates · 6 examples (`single_node` / `live_node` / `tool_agent` / `secure_agent` / `autonomous_agent` / `gateway`).

## 5. Deliberate gaps (filled in M2+)

- `fabric` is skeleton-only — agent↔agent collaboration, team orchestration, and CRDTs land in M4.
- Memory is an in-process `InMemorySpace`; a real vector store (e.g. Qdrant) and persistence come later.
- Capability signing currently accepts an injected `CapabilityVerifier`; production-grade key management is pending M2.
- Single process, no snapshot/restore — that is exactly the deliverable of **M2 microVM-ization**.

## 6. Next stop: M2 microVM-ization

Deliver F2/F3: one-click deploy + snapshot/restore + self-update rollback. Take this already-validated loop from M1
and package it into an isolatable, migratable, rollbackable runtime shell.
