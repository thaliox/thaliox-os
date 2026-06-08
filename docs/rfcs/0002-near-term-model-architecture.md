# RFC-0002 — Near-Term Model Architecture: Bounded-State Hybrid + MoE

| | |
|---|---|
| **Status** | Accepted |
| **Author** | THALIOX core |
| **Supersedes** | — |
| **Depends on** | [RFC-0001 (TAM)](0001-abstract-machine.md), [MASTER_PLAN.md](../MASTER_PLAN.md) |
| **Followed by** | [RFC-0003 (MELD)](0003-meld-cognitive-substrate.md) |

> **This RFC fixes the model architecture THALIOX builds on for H1 (M2–M5): a bounded-state hybrid backbone with a Mixture-of-Experts capacity layer.**
> It also defines the **Model-State Contract** in the `cognition` crate — the seam that lets any future architecture (including [RFC-0003 MELD](0003-meld-cognitive-substrate.md)) be hot-swapped without rebuilding the runtime.

---

## 1. Motivation

For a chat box, the choice of model architecture is a "few percent" engineering question. **For an AI-native OS it is a question of whether the roadmap can be delivered at all.**

THALIOX's differentiating milestones are stateful, OS-level operations on a *running* agent:

- **M2** — snapshot/restore + self-update rollback
- **M3** — live migration + CRDT merge + self-healing takeover

A standard Transformer's runtime state is its **KV-cache, which grows linearly and without bound** with context. A long-running agent's "mind" then weighs gigabytes and cannot be cheaply frozen, moved, or merged. This is not a performance nuisance — it is a **structural conflict** with the exact capabilities (M2/M3) that THALIOX intends to differentiate and fund on.

The TAM (RFC-0001 §6) already names the object we need: a **Checkpoint** = "the complete, recoverable state of an Agent", on which *snapshot / migration / merge / self-healing* are built. This RFC chooses an architecture whose runtime state **can actually be that Checkpoint**.

---

## 2. Decision

THALIOX H1 adopts a **Bounded-State Hybrid + MoE** model family:

1. **Bounded recurrent backbone** (SSM / linear-attention layers) carrying a **fixed-size, serializable state** per agent.
2. **Sparse full-attention layers** interleaved at a low ratio, preserving associative recall / copy that pure recurrent models lack.
3. **Mixture-of-Experts (MoE)** for capacity at small activated-parameter cost, sized to run inside a single microVM (M2).
4. **Adaptive compute** (variable readout work per step) exposed as a scheduler-controllable knob.

This RFC does **not** require self-training a model now. It requires that whatever model is integrated is wrapped by the **Model-State Contract** of §4, and that the bounded-state family is the integration target.

---

## 3. Architecture Specification

### 3.1 Bounded recurrent backbone + sparse attention

- The majority of layers MUST carry **constant per-token state** (e.g. SSM/Mamba-2 or gated linear attention), so that an agent's working state is a **fixed-size tensor** independent of context length.
- A minority of layers (or sliding-window attention) MAY be full attention, to retain in-context retrieval, exact copy, and multi-hop association. Pure recurrent backbones are **rejected** for the OS use case: agents depend heavily on reading tool outputs, citing history, and reconciliation — all attention-favoring.
- **Rationale → TAM:** the bounded state IS the *Working* tier of the Checkpoint (RFC-0001 §6); it makes `checkpoint` / `restore` O(state-size), not O(context).

### 3.2 MoE capacity layer

- Capacity scales with the number of experts; activated parameters stay small, keeping single-node (microVM) inference viable for M2.
- **Routing MUST be observable and gateable.** Expert routing and the number of activated experts are surfaced to the runtime so they can be **accounted against the AttentionBudget (INV-1)** and **bounded by policy**, not left as a black box.

### 3.3 Adaptive compute — the AttentionBudget knob

- The architecture MUST expose a **per-step compute control** (e.g. adaptive depth / early-exit / readout iterations) that trades compute for quality.
- This control is the architectural counterpart of the **AttentionBudget** (RFC-0001 §4): the scheduler allocates "thinking" to agents/tasks, and INV-1 deducts the real cost. It also gives M5's learned control plane a concrete actuator.

### 3.4 Native retrieval — co-designed with the `memory` crate

- External long-term memory MUST be treated as **architecture-level**, not a bolt-on RAG wrapper. The bounded state may **read/write the `memory` crate's SemanticSpace** (RFC-0001 §6) directly.
- Division of labour: **working memory = fixed-size state; long-term memory = external, addressable SemanticSpace.** This reinforces snapshot/migration: only the bounded state travels; the rest is shared, addressable storage.

---

## 4. The Model-State Contract (`cognition` crate)

The single most important deliverable of this RFC is **not** a specific model — it is the interface that makes the model's runtime state a first-class, OS-managed object, and makes every later architecture **hot-swappable**.

The `cognition` crate MUST define (shape illustrative, not final):

```rust
/// A model's runtime state is an OS-managed object, not a hidden buffer.
pub trait CognitiveState: Sized {
    /// Freeze to bytes — basis of TAM `checkpoint` (RFC-0001 §6).
    fn serialize(&self) -> StateBlob;
    /// Rebuild on this or another node — basis of restore / migration.
    fn restore(blob: &StateBlob) -> Result<Self, StateError>;

    /// Merge two diverged states (M3 CRDT merge / self-healing).
    /// MUST be defined; MAY return `Unsupported` for the near-term backbone,
    /// which is exactly the gap RFC-0003 (MELD) exists to close.
    fn merge(&self, other: &Self) -> Result<Self, MergeError>;

    /// One scheduled reasoning step under an explicit compute budget.
    /// Cost MUST be reported for INV-1 accounting.
    fn step(&mut self, input: &Event, budget: Budget) -> StepReport;

    /// Memory access is capability-gated (INV-2): no capability, no read.
    fn mem_read(&self, query: &Vector, cap: &Capability) -> Result<Vec<Object>, CapDenied>;
    fn mem_write(&mut self, obj: Object, cap: &Capability) -> Result<(), CapDenied>;
}
```

Contract rules:

- **C-1.** Any integrated model MUST implement `serialize`/`restore`; this is the precondition for M2.
- **C-2.** `step` MUST report real cost so INV-1 (budget conservation) holds end-to-end.
- **C-3.** `mem_read`/`mem_write` MUST enforce INV-2; the model cannot access memory it holds no capability for.
- **C-4.** `merge` MUST exist in the type system even when unimplemented. Near-term it MAY be `Unsupported`; **closing this gap is the mandate of RFC-0003.**

> **Insurance property:** because A (this hybrid), the experimental pillars of RFC-0003, and MELD itself all implement `CognitiveState`, they are interchangeable. This is what makes "any future architecture can land/integrate at any time" a build-time guarantee rather than a hope.

---

## 5. Mapping to TAM (RFC-0001)

| TAM concept | This RFC realizes it as |
|---|---|
| Checkpoint (§6) | `CognitiveState::serialize` of the **fixed-size** bounded state |
| Migration (§6) | `restore` of the state blob on the target node |
| Merge (§6) | `CognitiveState::merge` — `Unsupported` near-term; see RFC-0003 |
| AttentionBudget / INV-1 (§4) | adaptive-compute knob + MoE routing cost, reported by `step` |
| CapabilityToken / INV-2 (§5) | `mem_read`/`mem_write` gating |
| SemanticSpace (§6) | the external `memory` crate, read/written natively (§3.4) |

---

## 6. Mapping to Milestones

| Milestone | What this architecture unlocks |
|---|---|
| **M2** microVM-ization | fixed-size state ⇒ cheap, deterministic `checkpoint`/`restore`; rollback = restore prior blob |
| **M3** multi-instance HA | state blob is small and portable ⇒ live migration is tractable; `merge` gap flagged for RFC-0003 |
| **M5** learned control plane | adaptive-compute + routing are the actuators the RL scheduler controls |

---

## 7. Non-Goals

- This RFC does **not** commit THALIOX to self-training a foundation model in H1. Integrating an existing open hybrid model behind the contract is acceptable and expected for the M2 PoC.
- This RFC does **not** decide the long-horizon architecture; that is RFC-0003's exploratory mandate.
- This RFC does **not** specify the merge algorithm; near-term `merge` MAY be `Unsupported`.

---

## 8. Risks

1. **Hybrid retrieval ceiling** — too few attention layers can degrade hard retrieval/copy. Mitigation: treat the attention ratio as a tuned hyperparameter, validated on agent-style retrieval tasks, not perplexity alone.
2. **Ecosystem maturity** — kernels/quantization for hybrid/SSM lag Transformer. Mitigation: the contract isolates the runtime from the backbone; we can start on a Transformer-with-windowed-KV that *approximates* bounded state, then swap.
3. **State-size vs quality** — a too-small fixed state loses long-range fidelity. Mitigation: lean on §3.4 external memory for long-term recall; keep the state for working memory.

---

## 9. Open Questions

1. What attention-to-recurrent ratio actually preserves agent-critical retrieval at our scale?
2. What is the right serialized state-blob format for fast, deterministic restore across heterogeneous nodes (M3)?
3. How are MoE expert-activation costs converted into a uniform AttentionBudget unit (ties to RFC-0001 OQ#1)?

---

## 10. Conclusion

THALIOX does not choose a model by benchmark score. It chooses the architecture **whose runtime state the OS can treat as a snapshottable, migratable, gateable object** — because that, not perplexity, is what delivers M2 and M3. Bounded-State Hybrid + MoE meets that bar today, and the **Model-State Contract** ensures that the more radical RFC-0003 architecture can replace it without tearing down the runtime. The Transformer loses here at exactly its most famous feature: the unbounded KV-cache nobody can avoid.
