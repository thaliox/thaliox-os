# RFC-0006 — Cluster & multi-platform: agent↔agent, teams, distributed HA

| | |
|---|---|
| **Status** | Draft |
| **Author** | THALIOX core |
| **Supersedes** | — |
| **Depends on** | [RFC-0001 (TAM §3, §6)](0001-abstract-machine.md), [RFC-0005 (HA primitives)](0005-multi-instance-ha.md), [MASTER_PLAN.md](../MASTER_PLAN.md) |

> **This RFC designs M4 — many agents collaborating across many hosts.** M1 made
> one agent useful; M3 made an agent survivable but *single-host*. M4 builds the
> **fabric** that carries [`VectorMessage`](0001-abstract-machine.md)s (TAM §3)
> between agents and across nodes, turning M3's in-process HA primitives
> (`Node` / `migrate` / `Supervisor`) into a real distributed cluster, adding
> **teams** (holarchies) and **multi-platform clients**. Delivers F7/F8 → Series A.

---

## 1. Motivation

The differentiating moat is not a lone agent — it is **a fleet of agents that
communicate in vectors, organize into teams, survive node loss, and are reachable
from many clients.** M4 is the rung where THALIOX stops being a single-agent
runtime and becomes an *operating system for a society of agents*.

Two things already exist as skeletons since M1: the `fabric` crate (`Transport`
trait + `Team` + `Paradigm`) and `core`'s `VectorMessage`. M3 left two threads
explicitly for here: cross-host heartbeat/registry transport (RFC-0005 OQ4) and
cross-host migration consistency (OQ5). M4 picks them all up.

---

## 2. The fabric: VectorMessage transport (TAM §3)

`Transport` carries `VectorMessage`s — the unit in which agents exchange *meaning*,
not bytes (TAM §3). M4 gives the skeleton trait real implementations:

- **In-process** (M4a) — channels between agents on one node. CI-testable.
- **Networked** (M4b) — TCP/QUIC between nodes; serialized `VectorMessage`. The
  H2/H3 path is kernel-bypass / the `vsend`/`vrecv` silicon primitive.

**INV-3 (vector fidelity) is the fabric's law:** same `ModelFingerprint` ⇒ the
payload is delivered losslessly and MAY be injected directly into the receiver's
model; different fingerprint ⇒ it MUST pass an explicit translation with a
measurable loss metric — never an implicit lossy conversion. The fabric enforces
this at the boundary, not the agent.

Every cross-agent message is **capability-gated** (INV-2): a `VectorMessage`
carries an optional `CapabilityToken`; `Communicate` permission over the target
is required, exactly as `act` gates memory/tools.

---

## 3. From single-host HA to a distributed cluster

M3's `Supervisor` and `migrate` are correct but in-process. M4b runs them over the
networked `Transport`:

- **Distributed registry & heartbeat** — the supervisor's `observe`/`heartbeat`/
  `tick` flow over the fabric: nodes report liveness and ship their latest
  `Checkpoint` to the supervisor (or to peers) as control `VectorMessage`s.
- **Cross-host migration** — `migrate`'s `Package` bytes cross the network instead
  of a function call; the receiving node `LocalDeploy`s (or `FirecrackerDeploy`s)
  it. The flow is unchanged — only the transport differs. *(Already proven across
  two real microVMs on one host via vsock, RFC-0005 §7 M3b; M4b §7 ② extends the
  bytes over the network between two physical KVM hosts — capture-on-A →
  ship-Package → restore-into-a-fresh-VM-on-B, budget intact.)*
- **Self-healing across hosts** — the same migration triggered by a missed
  heartbeat, fenced by a supervisor epoch/capability (RFC-0005 OQ1).

This is where the KVM bare-metal earns its keep: **the cross-host HA validation
lands in M4b.**

---

## 4. Teams — a holarchy of agents

A `Team` is a *holarchy* (agents whole in themselves, composing into a larger
whole) executing a `Paradigm`:

| Paradigm | Coordination |
|---|---|
| **Hierarchy** | a lead agent delegates sub-goals; children report up |
| **Market** | agents bid on tasks; an auctioneer assigns by cost/fit |
| **Pipeline** | each agent transforms and forwards a `VectorMessage` to the next |
| **Swarm** | peers broadcast to an intent group; emergent consensus |

M4c implements team execution: spawn members, route `VectorMessage`s per the
paradigm, aggregate results. **All four paradigms are done** (§7 M4c), built on a
shared `Stage` abstraction over the fabric: `Pipeline` (chain), `Hierarchy`
(lead delegates → children report → lead aggregates), `Market` (`Bidder`s bid a
cost; lowest wins and executes), and `Swarm` (peers broadcast to an intent group
and fuse a consensus). INV-2 is enforced on every cross-member hop and INV-3 on
every agent's input — the team boundary is not a hole in the invariants.

---

## 5. Multi-platform clients (F8)

M1's `api` (axum HTTP gateway) is one client surface. M4d generalizes it so the
same agent fleet is reachable from multiple clients (HTTP/JSON for web & tools,
a streaming channel for live vector I/O, later native/SDK clients), all speaking
to the cluster through one authorization model (capabilities, INV-2). The gateway
becomes the cluster's front door, not a single agent's.

**Done** (`thaliox-api`, §7 M4d): the gateway gained a `cluster` mode on top of the
M1 request/response surface:
- **One authorization model at the door (INV-2)** — in cluster mode a request is
  admitted only if it carries a `CapabilityToken` (`x-thaliox-capability` header)
  granting `Communicate` over the target agent, checked *before* dispatch. This
  is uniform across every surface; open mode (default) admits all for local dev.
- **A second client surface** — `GET /agents/{id}/events` streams the agent's
  audit as Server-Sent Events (live I/O), through the same admission and routing.
- **Cluster routing / front door** — agents placed on a peer node (`place_remote`
  + `register_peer`) are answered with a `307` to that node's gateway, so one door
  fronts the whole fleet; `GET /cluster` reports the topology (node id, peers,
  local vs remote agents). A reverse-proxy variant (gateway forwards and returns
  the peer's response) is the next increment.

---

## 6. Mapping to TAM & milestones

| Concept | M4 realization |
|---|---|
| **VectorMessage** (TAM §3) | the `Transport` payload; INV-3 fidelity enforced at the fabric boundary |
| `vsend` / `vrecv` (TAM §3 / H3) | `Transport::send`/`recv` today; the silicon vector-transport NIC later |
| Cluster / Checkpoint (TAM §6) | distributed `Supervisor` + cross-host `migrate` (RFC-0005 over the fabric) |
| Capability (INV-2) | every cross-agent message is `Communicate`-gated |
| Teams (MASTER_PLAN §2.4) | `Team` + `Paradigm` execution |

---

## 7. Staged plan

| Stage | Deliverable | CI-gated? |
|---|---|---|
| **M4a** ✅ | agent↔agent over an **in-process** `Transport`. **Done** — `fabric::LocalFabric` routes `VectorMessage`s between `Endpoint`s (unicast + multicast); `send` is INV-2 capability-gated; `fidelity()` enforces INV-3 (Lossless / NeedsTranslation / Unaligned). | ✅ pure software (in CI) |
| **M4b** ✅ (CI) | **networked** `Transport` (TCP) + distributed `migrate`/`Supervisor`. **Distributed control plane done** — `fabric::NetNode`/`NetEndpoint` (VectorMessage over TCP, INV-2-gated), `send_migration`/`serve_migrations` (a `Package` migrates over TCP onto a remote node, state intact), and `send_heartbeat`/`serve_supervisor` (a node reports liveness + checkpoint; the `Supervisor` detects misses → suspected). All validated in CI over **real loopback TCP**. **Genuine two-machine run done** — a static-musl `fabric-node` binary (`crates/fabric-node`) ran the server on the bare-metal and the client on the dev host; an agent built on the dev host (budget 95) migrated over an SSH-tunnelled TCP connection and arrived on the remote node with its state intact. (Process-level cross-host — no KVM needed.) **② agent-in-microVM migration across two KVM hosts done** — `guest-runner fc-capture` on host A (43.133.119.80) booted a Firecracker microVM, deployed the demo agent, ran it down to budget 90, and pulled its `Checkpoint` over vsock into a 975-byte portable `Package`; the `Package` shipped host-A → dev-host → host-B over `rsync` (md5 identical end-to-end); `guest-runner fc-receive` on host B (43.152.240.102) booted a **fresh** microVM on a **second physical KVM machine** and restored the agent into it — health reported `budget=90` (continued, **not** reset to 100), proving full agent state survived two physical hosts and two independent microVMs. **③/④ cross-execution-context migration done** — because the migration unit is the execution-context-agnostic `Package`, the capture and restore halves recombine freely: `capture-local` (agent as a bare host process on A, budget→95) → `fc-receive` (fresh microVM on B) proves **physical→virtual** (budget arrived 90, one in-VM think); `fc-capture` (agent in a microVM on A, budget→95) → `receive-local` (bare host process on B) proves **virtual→physical** (budget arrived 95, lossless). The full migration matrix — {VM, host-process} source × {VM, host-process} dest, cross-host — is now green. | ✅ real-TCP tests (CI) + two real machines, full {VM,process}×{VM,process} matrix (self-hosted) |
| **M4c** ✅ | **teams**: all four `Paradigm`s implemented over the fabric on a shared `Stage` abstraction (each member's model forward-pass; a `MapStage` transform today). **Every cross-member hop is a real capability-gated fabric `send` (INV-2)** and the **INV-3 guard runs on entry to each agent** (a cross-`ModelFingerprint` aligned payload must be marked `Translate`, never implicitly injected). <br>• **Pipeline** (`fabric::Pipeline`) — a chain; each agent transforms and forwards. <br>• **Hierarchy** (`fabric::Hierarchy`) — a lead delegates the task to each child (INV-2 down) and aggregates their reports (INV-2 up). <br>• **Market** (`fabric::Market` + `Bidder`) — an auctioneer announces a task, bidders reply a cost over the fabric, the lowest bidder is assigned and executes. <br>• **Swarm** (`fabric::Swarm`) — peers broadcast a proposal to a shared intent group (capability-gated multicast) and fuse an emergent consensus. <br>Tested (fabric 9→18): each paradigm's happy path plus a negative — a mis-scoped hop/announce/broadcast capability is denied, and a cross-fingerprint hop is rejected unless translated. | ✅ in-process (CI) |
| **M4d** ✅ | **multi-platform clients**: the `api` gateway becomes the cluster front door (`thaliox-api`). **Done** — a cluster mode adds (1) **one authorization model (INV-2 at the door)**: requests must carry a `CapabilityToken` (`x-thaliox-capability`) granting `Communicate` over the target before dispatch; (2) **a second client surface**: `GET /agents/{id}/events` streams audit as Server-Sent Events (live I/O), same admission/routing; (3) **cluster routing**: peer-hosted agents get a `307` to their node's gateway and `GET /cluster` reports topology. Open mode (default) preserves M1 local-dev behavior. Tested (api 1→5): capability admission (missing/wrong → 403), 307 peer redirect with sub-path, SSE stream, topology. | ✅ in-process (CI) |

Start at **M4a** — the fabric transport is the foundation everything else (cluster,
teams, clients) rides on, and it is self-contained and CI-testable.

---

## 8. Open questions

1. Wire format for `VectorMessage` over the network — length-prefixed serde (reuse
   the `vmproto` framing from RFC-0004), or protobuf/gRPC for cross-language clients?
2. Vector translation (different `ModelFingerprint`) — where does the translation
   layer live (fabric boundary), and what is the canonical loss metric (cosine drift
   vs downstream-task fidelity, RFC-0001 OQ2)?
3. Membership & discovery — static node list for M4b, or gossip/SWIM for self-forming
   clusters?
4. Fencing for distributed self-healing — supervisor epoch in the registry, or a
   revoked `CapabilityToken` on the fenced instance (the TAM-native route, OQ1)?
5. Backpressure & ordering for streaming `VectorMessage` chunks (the `seq` field).

---

## 9. Conclusion

M4 turns THALIOX from a runtime for one survivable agent into an **operating system
for a distributed society of agents**: a fabric that carries meaning as vectors, a
cluster that keeps the fleet alive across hosts (cashing in M3's HA over the
network), teams that organize the agents, and a gateway that opens them to many
clients. It is built bottom-up — the fabric transport (M4a) first, everything else
on top — and it is where M3's HA finally runs cross-host (M4b, on real hardware).
