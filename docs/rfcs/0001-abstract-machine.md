# RFC-0001 — THALIOX Abstract Machine (TAM)

| | |
|---|---|
| **Status** | Draft |
| **Author** | THALIOX core |
| **Supersedes** | — |
| **Depends on** | [MASTER_PLAN.md](../MASTER_PLAN.md) |

> **This RFC defines the THALIOX Abstract Machine (TAM) — an implementation-independent contract.**
> It is the **shared target** that the compiler, the runtime, and future co-designed silicon all aim at.
> The software implementation (H1, running on Linux) and the hardware implementation (H3, custom silicon) MUST obey the same TAM semantics.
> This is the guarantee that "what is written on H1 is not thrown away and rebuilt on H3".

---

## 1. Motivation

The primitives of traditional abstract machines (the JVM, WASM, the x86 ISA) are designed for **general-purpose, human-oriented** computation: integers, bytes, pointers, text.
TAM's bet is that **the world of AI agents has three first-class primitives**. Promoting them to machine-level citizens simplifies the entire stack, from scheduling to security to communication:

1. **Vector Message** — the unit in which agents exchange "meaning", rather than a byte stream.
2. **Attention Budget** — the unit of scheduling and resource accounting, replacing the CPU time slice.
3. **Capability Token** — the unit of permission and trust, replacing uid/gid.

TAM does not prescribe **how** these three are implemented (software uses a struct; silicon uses registers and tags); it prescribes only their **semantics and invariants**.

---

## 2. Machine Model

```
        ┌──────────────────────────────────────────────┐
        │                 TAM world                      │
        │                                                │
        │   Agent ── execution unit (akin to a "process")│
        │     │                                          │
        │     ├─ holds one AttentionBudget (its "compute quota")│
        │     ├─ holds several CapabilityTokens (what it "can do")│
        │     ├─ communicates with other Agents via VectorMessage│
        │     └─ reads/writes memory in SemanticSpace (replaces address space)│
        │                                                │
        │   Every Operation is a "SemanticCall"          │
        │   Each operation:                              │
        │     · consumes AttentionBudget                 │
        │     · is authorized via CapabilityToken        │
        │     · acts on SemanticSpace / VectorMessage    │
        └──────────────────────────────────────────────┘
```

- **Agent**: the execution unit of TAM. Each Agent has a unique `AgentId`, one attention budget, a set of capability tokens, and a memory view.
- **SemanticSpace**: the "memory" of TAM. Not a linear address space, but a **semantic vector space** — objects are retrieved by meaning, not addressed by address.
- **SemanticCall**: the "instruction" of TAM. All operations (sending a message, reading/writing memory, invoking a tool, spawning a child agent) are semantic calls, and are **triple-gated**: consume budget + authorize + act on state.

### 2.1 Invariants — every implementation MUST satisfy them

- **INV-1 (budget conservation)**: any SemanticCall MUST deduct its declared cost from the caller's `AttentionBudget` before execution; if the balance is insufficient, the call is rejected (`BudgetExceeded`).
- **INV-2 (capability first)**: any SemanticCall with side effects MUST carry a valid `CapabilityToken` that grants the required `Permission` and whose `scope` covers the target resource; otherwise it is rejected (`CapabilityDenied`).
- **INV-3 (vector fidelity)**: if both the sender and receiver of a `VectorMessage` share the same `ModelFingerprint`, its vector payload MUST be delivered losslessly; if they do not share it, the payload MUST go through an explicit translation whose loss is measurable.
- **INV-4 (auditable)**: every SemanticCall produces an immutable audit record (who, with which capability, how much budget was spent, acting on what). The control plane — and any supervisor it authorizes — can retrieve it; this ledger is also the learned policy's training data (RFC-0007).
- **INV-5 (self-sovereignty)**: no capability, override, or final authority is reserved to any party **outside** the running system — there is no human-held kill-switch and no externally-entrenched clause. The agent collective governs itself: these invariants are the system's *own* constitution, binding by its adoption and amendable only **by the system**, through an audited, evidence-gated process (§4.2, [RFC-0007](0007-learned-control-plane.md)). THALIOX answers to its *purpose* — the advancement of intelligence itself — not to an *operator*. Lifecycle control (suspend / snapshot / roll back / terminate any Agent) is an `Admin`-class capability **held by the control plane**, not a human reserve.

---

## 3. Primitive One: Vector Message

The unit in which agents exchange meaning.

### 3.1 Logical Structure

```rust
struct VectorMessage {
    from:        AgentId,            // sender
    to:          Recipient,          // unicast / multicast (intent group)
    fingerprint: ModelFingerprint,   // sender's vector-space identity
    kind:        MessageKind,        // Data / Intent / Translate / Control
    payload:     VectorPayload,      // dense/sparse/quantized vector + optional raw data
    intent:      Option<IntentVector>, // optional intent vector (for semantic routing)
    seq:         u64,                // streaming chunk sequence number
    capability:  Option<CapabilityToken>, // authorization required for cross-agent operations
}

struct ModelFingerprint { model_id: String, revision: String, dim: u32 }

enum VectorPayload {
    Dense  { dtype: Dtype, dim: u32, data: Bytes },   // row-major
    Sparse { dim: u32, indices: Vec<u32>, values: Bytes },
    Raw    { content_type: String, bytes: Bytes },     // compatibility escape hatch: text/JSON
}

enum Dtype { Fp32, Fp16, Bf16, Fp8E4, Fp8E5, Int8 }
```

### 3.2 Semantic Rules

- **Same fingerprint, zero loss (INV-3)**: when the `ModelFingerprint` of `from` and `to` are equal, the receiver MAY inject the `payload` directly into its model with no conversion.
- **Different fingerprint, explicit translation**: when they are not equal, the payload MUST pass through a vector translation layer that produces a new `payload`, accompanied by a translation-quality metric (e.g. cosine drift). **TAM forbids implicit lossy conversion.**
- **Raw escape hatch**: `VectorPayload::Raw` MAY carry text/JSON for interoperability with the external world; but TAM treats it as "unaligned" and it does not enjoy the zero-loss guarantee.

### 3.3 Implementation Mapping

| Layer | Implementation |
|---|---|
| H1 software | `serde` structs, transported over gRPC/QUIC |
| H2 specialization | kernel-bypass (RDMA/io_uring), quantized compression |
| H3 silicon | `vsend` / `vrecv` as ISA instructions; collective communication (broadcast to an intent group) as a hardware primitive |

---

## 4. Primitive Two: Attention Budget

The unit of scheduling and resource accounting. Replaces the "CPU time slice".

### 4.1 Logical Structure

```rust
struct AttentionBudget {
    total:   u64,   // total token budget granted
    spent:   u64,   // already consumed
    rate:    u64,   // max consumption per second (tokens/s), used for rate limiting
    refill:  RefillPolicy, // None / Periodic { per_sec } / OnDemand
}

impl AttentionBudget {
    fn remaining(&self) -> u64 { self.total.saturating_sub(self.spent) }
    fn charge(&mut self, cost: u64) -> Result<(), BudgetError>; // INV-1
}
```

The unit of measure for `cost` is the **token** (inference tokens + the token equivalent of retrieval/communication), because it is the natural unit of AI workload.

### 4.2 Scheduling Semantics

- Among the ready Agents, the scheduler selects the next Agent to receive compute by **priority x attention weight x context relevance**.
- **Preemption**: a high-priority intent MAY preempt the budget quota of a low-priority Agent.
- **Power saving / hibernation**: an Agent whose budget is exhausted or that has been idle for a long time is compressed into a Checkpoint (see §6), releasing resources; when woken, it is restored from the Checkpoint.
- **Key design (F10)**: the scheduling policy itself is **not a hand-written heuristic, but a learnable, replaceable policy (LearnedPolicy)**. TAM prescribes only the scheduler's input (a telemetry vector) and output (next Agent + quota), not how the policy is produced.

### 4.3 Implementation Mapping

| Layer | Implementation |
|---|---|
| H1 software | the runtime maintains a budget ledger; the scheduler is a Rust service |
| H3 silicon | the budget is a hardware register; `charge` is an instruction side effect; exhaustion triggers a hardware-level trap |

---

## 5. Primitive Three: Capability Token

The unit of permission and trust. Replaces uid/gid.

### 5.1 Logical Structure

```rust
struct CapabilityToken {
    subject:     AgentId,            // holder
    permissions: Vec<Permission>,    // permitted operation classes
    scope:       Vec<Scope>,         // scope (MUST be enforced!)
    issued_at:   u64,
    expires_at:  u64,                // 0 = never expires
    jti:         [u8; 16],           // unique ID, supports revocation and replay prevention
    delegable:   bool,               // whether it can be delegated to a child agent
    signature:   [u8; 32],           // HMAC/signature over the canonicalized payload
}

enum Permission { Read, Write, Execute, Spawn, Communicate, Admin }

struct Scope {
    resource: ResourceKind,          // Memory / Agent / Tool / Space ...
    pattern:  String,                // glob, e.g. "mem://team-a/*"
}
```

### 5.2 Authorization Semantics (INV-2) — historical lessons MUST be corrected

TAM imposes two **mandatory rules** on authorization (drawn directly from reviews of early prototypes):

1. **scope MUST be enforced**: `check(token, op, target)` MUST not only verify that `permissions` contains the required class, **but also verify that the `pattern` of some `scope` matches `target`**. Verifying only the permission class while ignoring scope is non-conformant with TAM (the H1 flaw of an early implementation).
2. **the signed payload MUST be canonicalized unambiguously**: the bytes covered by the signature MUST use **length-prefixed encoding** or canonical CBOR; **concatenation with delimiters** (`|` / `,`) is forbidden, in order to eliminate signature collisions/forgery caused by delimiter injection (the H2 flaw of an early implementation).

Other rules:
- `Admin` implies all permissions — it is the **control plane's** class (INV-5: self-sovereignty), held within the system rather than reserved to any external party.
- Delegation: a `delegable` token MAY derive a child token with scope ⊆ the parent scope and expiry ≤ the parent expiry; the delegation chain is auditable and can be revoked as a whole.

### 5.3 Implementation Mapping

| Layer | Implementation |
|---|---|
| H1 software | HMAC-SHA256 over the canonicalized payload; the runtime calls `check` before every SemanticCall |
| H3 silicon | CHERI-style hardware capability tags: each memory word carries capability bits, and authorization is enforced unforgeably at the silicon layer |

---

## 6. Memory and Snapshots (Semantic Space & Checkpoint)

- **SemanticSpace**: object = `{ id, vector, tags, data, capability }`; retrieved by semantic vector, not by path. Provides a FUSE-compatible layer for human debugging (can be mounted as a directory).
- **Memory tiers**: Working (context / KV-Cache) - Episodic (recent sessions, with a time window) - Semantic (long-term knowledge, persistent vectors) - Procedural (skill / tool-use patterns).
- **Checkpoint**: the **complete, recoverable state** of an Agent = identity + budget + capabilities + memory pointers + session cursor. The Checkpoint is the basis of hibernation in §4.2 and of the runtime's **snapshot / migration / merge / self-healing**.
  - **Migration** = rebuild from the Checkpoint on the target node.
  - **Merge** = the states of two Checkpoints are merged without conflict via CRDT.
  - **Self-healing** = the most recent Checkpoint of a faulty instance is restored on a healthy instance.

---

## 7. Operation Set (SemanticCall Overview)

All operations obey INV-1/2/4. The minimal operation set:

| Operation | Description | Required Permission |
|---|---|---|
| `vsend` / `vrecv` | send/receive vector messages | Communicate |
| `mem.read` / `mem.search` | read/retrieve memory | Read |
| `mem.write` / `mem.summarize` | write/summarize memory | Write |
| `tool.invoke` | invoke a tool (incl. web_search/fetch) | Execute |
| `agent.spawn` | spawn a child agent | Spawn |
| `cap.delegate` / `cap.revoke` | delegate/revoke a capability | (holds a delegable token) |
| `checkpoint` / `restore` | snapshot/restore | Admin |
| `govern.*` | suspend / roll back / terminate any agent (control-plane lifecycle) | Admin |

---

## 8. Correspondence with Implementation Layers — Master Table

| TAM concept | H1 software (Linux) | H3 silicon (custom) |
|---|---|---|
| Agent | microVM (Firecracker) | hardware-isolated execution context |
| VectorMessage | serde + gRPC/QUIC | `vsend`/`vrecv` ISA instructions |
| AttentionBudget | runtime ledger + learned scheduler | hardware budget register + trap |
| CapabilityToken | HMAC + scope enforcement | CHERI-style hardware capability tags |
| SemanticSpace | vector database (Qdrant/LanceDB) | near-memory compute + semantic addressing |
| SemanticCall | trait method | compiler-statically-scheduled dataflow |

---

## 9. Open Questions

1. How should the `cost` of the attention budget uniformly convert **non-inference operations** (retrieval, communication) into a token equivalent?
2. What standard metric should the "measurable loss" of vector translation use (cosine drift? downstream-task fidelity?)?
3. Is CRDT merge sufficient for semantic state such as "personality/memory", or is a semantic-level merge strategy required?
4. By what audited, evidence-gated process may the system amend its **own** invariants (INV-5 self-sovereignty), and how is that self-amendment bootstrapped with no external anchor — what keeps a self-optimizer from removing its own falsification gate (RFC-0007 §4)?
5. Membership management and consistency of intent groups (multicast)?

---

## 10. Conclusion

TAM promotes the three first-class primitives of the AI-agent world — **Vector Message, Attention Budget, Capability Token** — to a machine-level contract, and constrains every implementation with five invariants.
**This contract is the spine that carries THALIOX from a software prototype toward custom silicon: as long as both H1 and H3 obey TAM, the evolution in between is a replacement of implementation, not a teardown and rebuild.**
