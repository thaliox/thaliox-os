# RFC-0008 — Pushing the stack down: H2 begins

| | |
|---|---|
| **Status** | Draft |
| **Author** | THALIOX core |
| **Supersedes** | — |
| **Depends on** | [RFC-0001 (TAM contract, §4.2 mechanism/policy, INV-1..5)](0001-abstract-machine.md), [RFC-0004 (Firecracker guest, vmproto)](0004-firecracker-deploy.md), [RFC-0006 (the TCP fabric being replaced)](0006-cluster-multiplatform.md), [RFC-0007 (the metered control plane that consumes the new telemetry)](0007-learned-control-plane.md), [MASTER_PLAN §3 (L0 substrate), §5 (hardware vision), §6 (M6)](../MASTER_PLAN.md) |

> **This RFC designs M6 — the first milestone of H2, where THALIOX stops building
> *on top of* Linux and starts taking the substrate *away from under it*.** H1
> (M1–M5) proved the TAM contract upward: agents, microVMs, HA, clusters, and a
> learned control plane, all invariant-guarded — but every one of those layers
> still rides a general-purpose substrate built for *not knowing what will run
> next*. THALIOX knows exactly what runs next: predictable, token-metered,
> capability-scoped agent dataflow. Everything the general-purpose stack does to
> cope with unpredictability — dynamic scheduling, per-call kernel crossings,
> POSIX surface, copy-through-the-kernel networking — is **pure overhead by the
> Clean-Slate Mandate** (founding principle 4: *the purpose is efficiency*). M6
> sheds it in four measured steps: **meter the tax (eBPF) → shrink the guest to
> the TAM contract (unikernel-style) → take the kernel out of the vector path
> (bypass transport) → put the first TAM primitive into hardware (FPGA)**. The
> H2 deliverable is not a feature — it is **a falsifiable efficiency curve**,
> each point earned the way E1–E5 were earned: against a measured baseline, or
> not at all.

---

## 1. Motivation

The L0 row of MASTER_PLAN §3 reads: *"Now: Linux + KVM + cgroups + namespaces +
eBPF. Endgame: THALIOX Abstract Machine + co-designed silicon."* H1 deliberately
parked the substrate question — Linux was the fastest way to prove the contract
upward, and every mechanism (deploy, snapshot, migrate, heal, fabric, govern) was
built against **TAM interfaces, not Linux interfaces**, precisely so that the
floor could later be swapped without tearing the house down.

M6 starts the swap. Three observations make it the right time:

1. **The contract above is closed.** M1–M5 shipped the full loop — an agent
   society that runs, heals, migrates, clusters, and governs itself. The TAM
   surface is stable (RFC-0001 has survived five milestones unchanged); what sits
   *below* it is now the dominant unexplained cost.
2. **The control plane is ready to consume substrate telemetry.** M5's governor
   observes the cluster as a vector and learns policy from the INV-4 ledger
   (RFC-0007). But its state vector today sees only what userland sees — budgets,
   health, load. The substrate tax (kernel crossings, copies, context switches)
   is invisible to it. M6a makes that tax a first-class, attributable signal.
3. **Every H2 claim must be a measured claim.** "A real efficiency curve" is the
   H2 deliverable (MASTER_PLAN §6) — investors, contributors, and the project
   itself only learn something when a replacement *beats a number*. The E1–E5
   discipline (kill-gates against baselines) extends downward unchanged.

**The iron rule of M6: no layer is deleted before it is metered, and no
replacement ships before it beats the meter.** Legacy is not removed because it
is legacy; it is removed because the ledger shows it taxing the dataflow, and
the replacement demonstrably stops the bleeding.

---

## 2. The method: meter → replace → verify

Each M6 stage follows one loop:

```
meter the substrate tax (baseline B)
        │
        ▼
replace the taxed layer with a TAM-shaped one (candidate C)
        │
        ▼
gate: C beats B on the declared metric, invariants intact → ship
      C fails → keep Linux's version, keep the meter, try again
```

This is TAM §4.2 applied to the substrate: the **mechanism** (run an agent,
carry a vector, verify a capability) is fixed by the contract; *how the
substrate implements it* is a swappable policy, and swaps are won by
falsification, not by ideology. A Linux subsystem that beats our replacement
**stays** — that, too, is the Clean-Slate Mandate, read honestly: efficiency is
the purpose, not novelty.

---

## 3. M6a — meter and enforce at the substrate (eBPF)

Two deliverables, one toolchain. eBPF is chosen deliberately: it is the one
piece of the legacy stack that lets us *instrument and constrain the legacy
stack from inside, without forking the kernel*.

### 3a. The substrate ledger (attribution)

eBPF probes (tracepoints + kprobes, CO-RE) attribute substrate events to TAM
operations:

- per **SemanticCall**: syscall count, context switches, on-CPU ns, kernel
  crossings;
- per **VectorMessage hop** (fabric send/recv): copies, wakeups, bytes through
  the kernel;
- per **mechanism actuation** (snapshot / migrate / heal / deploy): I/O issued,
  page-cache traffic, stop-the-world time.

The probes emit `(agent, tam_op, substrate_cost)` records into a ring buffer; a
userland collector joins them with the INV-4 audit stream — **the audit ledger
gains a substrate-cost column**. This is the same move M5 made with budgets
(RFC-0007 §1: "the OS was already instrumented for learning by its own
invariants"), extended downward: the control plane's state vector can now carry
*kernel-crossings-per-token*, and a learned policy can be trained to minimize
it. The efficiency curve of H2 is drawn from this ledger and nothing else.

### 3b. Capability enforcement below userland

INV-2 today is enforced in the runtime (every `act` checks the
`CapabilityToken`) and at the cluster door (RFC-0006). M6a compiles each
agent's capability scope into a **seccomp/LSM-eBPF policy attached to the agent
process**: an agent whose tokens grant no `net.*` scope cannot *make* a connect
syscall — the kernel refuses it before the runtime is even consulted. The TAM
contract becomes defense-in-depth: forging past the runtime check now also
requires forging past the kernel. Denials are reported upward into the same
audit stream (INV-4).

This does **not** replace the runtime check (the runtime knows semantics the
syscall layer cannot), and the userland check remains authoritative for
*grants*; the substrate layer is a deny-only floor… and per INV-5 it is held
and updated by the system itself: the policy compiler is driven by the same
capability state the control plane already governs — no human writes filter
rules.

---

## 4. M6b — the guest *is* the contract (unikernel-style guest)

The M2 guest (RFC-0004) boots a general-purpose Linux guest image whose rootfs
carries `thaliox-guest-runner`, and the host speaks vmproto to it over vsock.
It works — and it drags an init system, userland plumbing, and a POSIX surface
into every microVM, none of which any agent ever calls. The guest's *actual*
interface to the world is exactly **vmproto over vsock** — the TAM contract.

M6b makes that literal (F11: *abstract-machine contract first*):

- **`guest-runner` becomes PID 1** on a minimal kernel: no init, no shell, no
  users, no TTY, no network stack (vsock only), read-only rootfs measured in
  megabytes;
- the kernel config is pruned to what the runner's syscall profile — as
  *measured by M6a*, not guessed — actually needs;
- the vmproto conformance suite (deploy / act / snapshot / restore round-trips,
  RFC-0004 §4) is the **unchanged** acceptance bar: the contract holds, the
  baggage goes.

What is measured (against the M2 guest on the same host): cold boot to
vmproto-ready, guest RSS at idle, unique syscalls observable from the guest,
image size. This is the "unikernel / abstract-machine contract" leg of the H2
row — not adopting a unikernel framework for its own sake, but **shrinking the
guest until the TAM contract is the only surface left**.

---

## 5. M6c — kernel-bypass vector transport

The M4 fabric (RFC-0006) carries `VectorMessage`s over TCP — every hop costs
syscalls, copies, and scheduler wakeups in both kernels. For the OS whose unit
of meaning is the vector, the vector path is the data plane; it gets the same
treatment a serious network OS gives packets:

- **Rung 1 — same-host shm ring**: agents co-located on a node exchange
  vectors through a shared-memory ring (the placement information M5 already
  has decides eligibility); a vector between neighbors crosses **zero** kernel
  boundaries.
- **Rung 2 — io_uring batching**: the cross-host TCP path moves to io_uring
  with registered buffers — syscalls amortized across batches instead of paid
  per message.
- **Rung 3 — AF_XDP (stretch)**: the host fabric daemon owns the NIC queue and
  the kernel network stack leaves the path entirely. Only attempted if rung 2's
  metered ceiling justifies it.
- **Rung 4 — memory-semantic fabric (CXL, horizon)**: on hosts joined by a
  CXL 3.0+ shared-memory window, the *same ring as rung 1* is established over
  a cross-host mapped region — a `VectorMessage` hop becomes cache-coherent
  load/store at a few hundred nanoseconds, and the NIC leaves the path the way
  the kernel left it in rung 3. The industry is converging on exactly this
  substrate (the interconnect standards war ended with CXL absorbing Gen-Z /
  OpenCAPI / CCIX; CXL 4.0 targets multi-rack coherent pools), which means H2
  may get "the network dissolves into the memory hierarchy" as a commodity
  floor rather than a bespoke one. This rung is **horizon-tracked, not gated
  in M6**: multi-host CXL sharing reaches commodity availability ~2026-2027,
  and no hardware is rented or bought for it before the curve justifies the
  spend (§9).

Rung 4 is why rung 1 carries a hard constraint, binding now: **the ring is
defined over *any* mapped memory window — never over same-host shm
specifically**. Establishment is a capability-gated mapping (INV-2), fidelity
guards sit at the ring edges (INV-3), and nothing in the ring protocol may
assume which medium backs the mapping. "Same ring code, different window" is
the acceptance test that keeps rung 4 reachable without a rewrite.

Until real CXL hardware exists, a **functional probe** keeps that claim honest
and CI-able: QEMU emulates a CXL Type-3 memory device (Linux `dax`/`kmem`
surfaces it as a memory-only NUMA node), the rung-1 ring runs over that window,
and the fabric conformance suite must stay green; the substrate ledger gains a
memory-tier column so that, when real tiers arrive (CXL memory runs 2-5×
slower than local DRAM), M5's placement policy sees tier cost the same way it
sees every other substrate cost. Emulation yields **functional evidence only**
— no efficiency verdict is ever drawn from emulated latency, the same honesty
rule E6 applies to captures (*no verdict until live*).

Invariants are not relaxed: INV-2 admission and INV-3 fidelity guards stay at
the fabric door exactly as RFC-0006 placed them — the rungs change *who moves
the bytes*, never *who is allowed in*. Measured against the M4 TCP baseline on
identical hardware: messages/sec/core, p50/p99 latency, bytes copied per
message (from the M6a ledger).

---

## 6. M6d — the first TAM primitive in hardware (FPGA)

H3 (M7) tapes out a primitive; M6d de-risks it for the price of a dev board.
The chosen primitive is **capability verification** (INV-2): HMAC over the
canonical length-prefixed token encoding (RFC-0001, `thaliox-cap`) — small,
stateless, latency-critical (it sits on every call path and every fabric
admission), and exactly the kind of fixed-function logic FPGAs reward.

Deliverable: a verify core on a dev board (PCIe or USB-attached), fed by the
host fabric; the gate measures **verifications/sec/watt and fixed-latency
distribution vs the CPU implementation** on the same node. Success here is the
first physical evidence for MASTER_PLAN §5.2's "hardware capability security"
pillar — and failure is cheap and instructive, which is the point of doing it
two milestones before silicon.

---

## 7. Staged plan

| Stage | Deliverable | Gate (falsification) | Where it runs |
|---|---|---|---|
| **M6a** 🟡 | the **substrate ledger** (eBPF attribution of syscalls / crossings / copies to TAM ops, joined to the INV-4 audit) + **INV-2 compiled to seccomp/LSM-eBPF** per-agent deny floors — **CI leg shipped** (`thaliox-substrate`): `SubstrateLedger` joins probe samples to the INV-4 audit stream by (agent, op, time-window) with unattributed samples counted, never dropped; per-op `Baseline`s with the E6 reproducibility measure; the `Probe` contract + `ReplayProbe` (JSONL captures, the CI-side probe; live eBPF probe lands behind the `ebpf` feature on the box); the **E6 harness** (`experiment::e6` — overhead + reproducibility, no verdict until live captures exist); and the **deny-floor compiler** (`compile_deny_floor`: capability scope → coarse `SyscallGroup` warrants → deny-only OCI seccomp profile; monotone — more capability never means more denial; the Ptrace/ModuleLoad/MountNs tail warranted by nothing short of Admin). *Live leg pending bare-metal.* | **E6**: the meter itself costs < 3% throughput at full attribution, and per-op substrate baselines are reproducible across runs (< 10% variance) — *no later gate exists until this one passes, because every later gate divides by these numbers* | bare-metal KVM host (probes need a real kernel); logic + policy-compiler unit tests in CI |
| **M6b** | the **contract guest**: `guest-runner` as PID 1, pruned kernel, vsock-only, vmproto conformance unchanged | **E7**: ≥ 3× faster cold-boot-to-ready, ≤ ½ idle RSS, and a strictly smaller measured syscall surface vs the M2 guest — same conformance suite green | bare-metal KVM host; image build + conformance suite scripted, CI-runnable on any KVM runner |
| **M6c** | **vector data plane**: same-host shm ring → io_uring batched cross-host (→ AF_XDP stretch), behind the existing fabric interface; the ring contract is **medium-agnostic** (§5), proven by the emulated-CXL functional probe (rung 4 is horizon-tracked — functional evidence only, no efficiency verdict) | **E8**: ≥ 2× messages/sec/core *or* ≤ ½ p99 vs the M4 TCP baseline on identical hardware, with INV-2/INV-3 enforcement intact at the door (zero admission regressions in the conformance suite) | rung 1 partially CI-able (shm, single host); rungs 2–3 bare-metal; rung 4 probe CI-able (QEMU CXL emulation) |
| **M6d** | **FPGA capability-verify core** fed by the host fabric | **E9**: ≥ 10× verifications/sec/watt *or* tighter-than-CPU fixed latency (p99.9), bit-exact agreement with `thaliox-cap` on the E3 vector suite | FPGA dev board (hardware purchase) + host |

Order matters: **M6a is first and non-negotiable** — it is the meter every
other stage is judged by (and its absence is how legacy survives unexamined).
M6b/M6c can proceed in parallel once E6 passes; M6d is independent and gated
only on the board.

Exact gate factors (3×, ½, 2×, 10×) are **provisional until E6 locks the
baselines** — they are then frozen into the experiment harness the way E1–E5
were (`crates/*/src/experiment/`), and a stage that cannot beat its frozen gate
does not ship.

---

## 8. Mapping to TAM & the master plan

| Concept | M6 realization |
|---|---|
| **Clean-Slate Mandate** (principle 4) | legacy is shed *by measurement*: every deleted layer has a ledger entry proving it taxed the dataflow, every replacement a gate proving it stopped |
| **INV-2 capability-first** | enforced at a third layer (kernel deny-floor, M6a) and prototyped in hardware (M6d) — same tokens, same scopes, deeper roots |
| **INV-4 auditable** | the audit ledger gains the substrate-cost column; the efficiency curve *is* a view over INV-4 data |
| **INV-5 self-sovereignty** | seccomp/LSM policies are compiled from capability state the control plane governs — no human writes the filter rules |
| **VectorMessage** (TAM §3) | gets a data plane worthy of a primitive: zero-crossing same-host, batched cross-host (M6c) |
| **AttentionBudget** (TAM §3) | the ledger prices substrate cost *per token*, so M5's learned policies can optimize kernel-crossings-per-token like any other efficiency term |
| Mechanism/policy split (TAM §4.2) | substrate implementations are swappable policies; swaps won by falsification (E6–E9), Linux keeps any round it wins |
| F10 (OS dissolves into the compiler) | M6b's pruned, statically-known guest is the first concrete deletion of dynamic-substrate generality |
| F11 (abstract-machine contract first) | M6b ships a machine whose **entire OS interface is the TAM contract** (vmproto over vsock) |
| M7/H3 (silicon) | M6d is the cheap rehearsal: one primitive, real gates, real watts — and rung 4 names the **capability memory controller** candidate's nearest concrete form: a CXL Type-3 device enforcing INV-2 *in the controller*, riding the commodity CXL fabric (and its PCIe physical layer) instead of a bespoke interconnect |
| Migration / snapshot (RFC-0004/0006) | horizon note: on a pooled memory-semantic fabric, migrating an agent whose state lives in the pool degenerates from *copying state* to *remapping ownership* — the substrate-side path to MASTER_PLAN §5.2's "hardware-native snapshot/restore" |

---

## 9. Hardware & CI strategy

- **CI (always green, no hardware)**: all crates compile; policy-compiler,
  ledger-join, shm-ring, and conformance-suite logic unit-tested; gate
  harnesses run in *replay mode* against committed baseline captures.
- **Bare-metal KVM host** (M6a/b/c live numbers): the M2/M4 pattern — a
  dedicated box, smoke-tested scripts, results committed as the baselines CI
  replays. The previously used KVM hosts were released after M4; **a box is
  re-rented when M6a implementation starts** (operator action — a spend
  decision, not a control decision).
- **FPGA dev board** (M6d): one mid-range board (e.g. Artix/ECP5-class is
  enough for an HMAC core); purchase deferred until E6/E7 are green so the
  curve funds the board's claim. If M6d results point at the capability memory
  controller as the M7 primitive, a CXL-hard-IP board (Agilex-class) is the
  natural *second* board — a later, separate spend decision, never a
  prerequisite for E9.
- **CXL (M6c rung 4)**: emulation only during M6 — QEMU's CXL Type-3 model
  costs nothing and runs in CI; real multi-host CXL sharing is deferred until
  commodity hardware exists (~2026-2027), and renting/buying it is, as always,
  a spend decision the curve must justify first.

---

## 10. Open questions

1. **Attribution granularity vs overhead** — per-SemanticCall attribution is
   the goal; if E6's 3% budget cannot hold at full resolution, what sampling
   strategy degrades gracefully without blinding the curve?
2. **Guest kernel floor** — how far down does the pruned config go before
   Firecracker/virtio constraints push back? Is a non-Linux unikernel base
   (e.g. a minimal Rust kernel) a *later* rung, and what would its conformance
   gate be?
3. **shm-ring trust boundary** — same-host rings bypass the fabric door between
   co-located agents; INV-2 admission must move to ring *establishment* (a
   capability-gated mmap), and INV-3 guards to the ring edges. Is establishment
   revocable mid-stream when a token expires?
4. **Learned placement meets bypass** — M5's policy can now prefer co-location
   to exploit rung 1. Does the placement reward term come from the substrate
   ledger directly, and does that create a feedback loop the E5 gate must
   re-validate?
5. **eBPF verifier limits** — complex per-agent scope → filter compilation may
   hit program-size/verifier ceilings; is the fallback a coarser deny-floor
   plus userland authority (acceptable), or tail-called program chains?
6. **FPGA primitive choice** — capability-verify is the conservative pick; is
   VectorMessage switching (a tiny crossbar with admission) the bolder one that
   teaches more about M7's NoC, and can the board host both?
7. **Revocation on a load/store medium** — on a socket, revoking admission
   tears down a connection the kernel mediates; on a shared window (shm today,
   CXL tomorrow) bytes move with no per-message doorman. Same-host, unmapping
   is the host's privilege; cross-host CXL unmapping requires fabric-manager
   cooperation. Does question 3's answer (revocable establishment) survive a
   medium where no kernel sits between peers — or does rung 4 require a
   hardware-assisted revocation primitive, which would itself be an M7-grade
   argument for the capability memory controller?

---

## 11. Conclusion

M6 turns THALIOX's vertical ambition from a diagram annotation ("L0: replaced
downward by stage") into the same disciplined loop that built M1–M5: **meter,
replace, verify**. eBPF gives the system eyes below userland and a third
enforcement layer for INV-2; the contract guest shrinks a microVM until the TAM
contract is its entire surface; the bypass data plane lets vectors move at the
speed the primitive deserves; and one FPGA core makes the hardware conversation
empirical two milestones early. Every step is gated (E6–E9) against numbers a
skeptic can re-run, and any round Linux wins, Linux keeps — because the purpose
is efficiency, and the curve, not the slogan, is the deliverable that carries
THALIOX from H1's finished software arc toward H3's machine that belongs to AI.
