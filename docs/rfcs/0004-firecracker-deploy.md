# RFC-0004 — FirecrackerDeploy: the microVM launch target

| | |
|---|---|
| **Status** | Draft |
| **Author** | THALIOX core |
| **Supersedes** | — |
| **Depends on** | [RFC-0001 (TAM)](0001-abstract-machine.md), [RFC-0002 (Model-State Contract)](0002-near-term-model-architecture.md), [MASTER_PLAN.md](../MASTER_PLAN.md), [M2-PROGRESS](../M2-PROGRESS.md) |

> **This RFC designs the Firecracker realization of the M2 deployment unit.** The software layer already defines the seam — `Package`, `DeployTarget`, and the in-process `LocalDeploy` (RFC-0002 §4 / `runtime::package`). `FirecrackerDeploy` is a sibling launcher that runs the agent **inside a microVM** and returns a `MicroVm` handle (see §6/§9).
> **Status: all stages (host smoke + F2a–F4) implemented and validated on real KVM hardware** — see §9. The host provisioning runbook and evidence are in §10.

---

## 1. Motivation

M2's third leg is *one-click deployment*, and its most ambitious form is a real microVM boundary (MASTER_PLAN §6, F2/F3). `LocalDeploy` proved the interface; Firecracker makes the isolation real — a hardware-virtualized, <125 ms-boot, ~5 MiB-overhead VM per agent.

The seam is already cut: every launcher is a `DeployTarget` (RFC-0002 §4). So this RFC is **not** about reshaping the data model — `Package` and `Manifest` are unchanged. It is about the two new pieces a VM boundary forces:

1. a **guest agent-runner** — what executes *inside* the VM, and
2. **`FirecrackerDeploy`** — the host-side launcher that boots the VM and feeds it the `Package`.

And the one decision that shapes both: **how the `Package` crosses the host→guest boundary.**

---

## 2. Architecture

```
        host (THALIOX runtime)                        guest microVM
  ┌─────────────────────────────┐            ┌──────────────────────────────┐
  │ FirecrackerDeploy            │  vsock     │ agent-runner (PID 1)         │
  │  · spawn firecracker         │ ◀────────▶ │  · read Package over vsock   │
  │  · configure kernel/rootfs   │            │  · LocalDeploy in-VM         │
  │  · attach vsock              │  Package   │  · run Agent (TAM gates)     │
  │  · InstanceStart             │  ───────▶  │  · checkpoint on request     │
  │  · health / checkpoint pulls │  results   │  · stream audit / health     │
  └─────────────────────────────┘ ◀───────   └──────────────────────────────┘
        DeployTarget::deploy                       runs RFC-0001 TAM contract
```

`FirecrackerDeploy::deploy(package, env)` returns a handle to a **running microVM** whose guest is executing the agent. The host keeps a control channel (vsock) for health checks, on-demand agent checkpoints, and (M3) migration.

---

## 3. Two snapshot layers — keep the portable one canonical

Firecracker has its *own* VM-level snapshot (a ~RAM-sized memory file + a small state file). THALIOX already has an *agent-level* checkpoint (RFC-0002 §4, portable + serializable + mergeable-later). They are **different layers and must not be conflated**:

| Layer | Artifact | Scope | Portable? | Role |
|---|---|---|---|---|
| **Agent checkpoint** (canonical) | `Package` (Manifest + `Checkpoint` blob) | the agent's bounded state | ✅ across nodes / kernels / architectures; mergeable later (RFC-0003 P2) | **the unit of deploy / migrate / rollback** |
| **Firecracker VM snapshot** (optimization) | `memory.file` + `snapshot.file` | the whole guest RAM + device state | ❌ host/kernel-specific, ~512 MiB | fast *local* hibernate / resume of a live microVM |

> **Rule:** the **agent checkpoint is the source of truth.** `FirecrackerDeploy` deploys and migrates via the `Package`; Firecracker VM snapshots are a *performance layer* for suspending/resuming a running VM on the same host. The runner can mint an agent checkpoint on demand (over vsock) regardless of whether a VM snapshot exists — so our portable, mergeable `Checkpoint` never depends on a host-specific memory image.

---

## 4. Decision: how the `Package` enters the guest

| Channel | Mechanism | Pros | Cons | Verdict |
|---|---|---|---|---|
| **vsock** | virtio-vsock UDS on host ↔ port in guest | bidirectional (results / health / checkpoint pulls back); large payloads; no per-deploy image rebuild; foreshadows TAM VectorMessage transport / the H3 vector-transport NIC | guest needs a vsock client; a little setup | ✅ **target** |
| config-drive | Package written to a small block image, attached as `/dev/vdb` | dead simple, no networking | one-shot, read-only, rebuild per deploy, **no return channel** | bootstrap only |
| MMDS | Firecracker metadata service over link-local HTTP | built in, no extra drive | size-limited (metadata, not state blobs); needs guest net | ✗ |

**Decision: vsock is the target channel.** It is the only option with a **return path**, which M2 health-checks and M3 migration both need, and it scales to large checkpoint blobs. To de-risk runner bring-up, **F2 starts on config-drive** (one-way, trivial) and **switches to vsock in F2b** once the runner boots — the `Package` bytes are identical, only the transport changes.

---

## 5. The guest agent-runner

A small Rust binary, baked into the rootfs, run as the guest's init (`init=/sbin/thaliox-runner` or PID 1). It MUST:

1. **Receive** the `Package` (config-drive in F2; vsock in F2b).
2. **Deploy in-VM** via `LocalDeploy` (RFC-0002): validate the manifest against the in-guest environment, `Agent::restore`, re-attach tools/verifier.
3. **Run** the agent under the full TAM gate (INV-1/2/4 unchanged — the VM boundary is orthogonal to the contract).
4. **Serve control requests** over vsock: `health`, `checkpoint` (mint a fresh `Package` and stream it back), `shutdown`.
5. Be **panic-safe**: a crash signals the host (vsock close / non-zero exit), which the host treats as an unhealthy deploy (rollback via RFC-0002 `conclude_update`).

The runner reuses `runtime`'s existing `LocalDeploy` verbatim — *inside* the VM, deploying is exactly the in-process case. No new deploy logic in the guest.

---

## 6. Host-side `FirecrackerDeploy`

Implements `DeployTarget` (RFC-0002 §4). `deploy(package, env)`:

1. Allocate a jail/work dir on fast storage (`/mnt/data`, see §10), drop in kernel + rootfs (copy-on-write where possible).
2. Spawn `firecracker --api-sock …` (optionally under `jailer` for seccomp/cgroup/chroot isolation).
3. Configure via the API: `boot-source`, `drives` (rootfs; +config-drive in F2), `machine-config`, `vsock`.
4. `InstanceStart`; hand the `Package` to the runner (config-drive content / vsock send).
5. Health-check over vsock; on failure tear down and surface an error (the caller's `conclude_update` rolls back).
6. Return a `MicroVm` handle (api socket, vsock path, pid) exposing `health()`, `checkpoint() -> Package`, `pause()/resume()` (Firecracker VM snapshot), `shutdown()`.

**Feature-gated** behind `--features firecracker` so the default build (and the pure-cargo CI) never pulls VM machinery. `LocalDeploy` remains the always-available target.

---

## 7. Mapping to TAM & milestones

| Concept | This RFC |
|---|---|
| Checkpoint (RFC-0001 §6) | `Package` carries it; the runner mints it on demand over vsock |
| Migration (RFC-0001 §6) | deploy a `Package` produced on node A onto a fresh microVM on node B — the M3 primitive, foreshadowed |
| Agent / isolation (TAM §8: "Agent → microVM") | realized literally: one agent per Firecracker microVM |
| AttentionBudget / capabilities | unchanged — enforced by the agent *inside* the VM |
| Fast hibernate/resume | Firecracker VM snapshot via the `MicroVm` handle (perf layer, §3) |

---

## 8. Test & CI strategy

- **Pure-cargo CI is unchanged** — it covers all the software layer (`LocalDeploy`, packaging, snapshot/restore, rollback). Firecracker code is `#[cfg(feature = "firecracker")]` and not compiled there.
- **Firecracker integration tests are self-hosted** on a KVM box (the Thailand host, §10), gated by `--features firecracker` + an env guard (e.g. `THALIOX_FC_KERNEL` / `THALIOX_FC_ROOTFS` paths). They are `#[ignore]`-by-default so a stray `cargo test` never tries to boot a VM.
- A thin **shell smoke test** (the §10 sequence) stays in the repo under `tools/` as the host-readiness check, independent of Rust.

---

## 9. Staged plan

| Stage | Deliverable |
|---|---|
| **F2a** ✅ | guest `agent-runner` (Rust): read `Package` from config-drive → `LocalDeploy` → run agent; baked into an ext4 rootfs. **Done — validated in-VM on the KVM host**: a 1.3 MiB static-musl runner boots as PID 1, reads `/dev/vdb`, deploys the agent (phase `Live`, budget 100), runs a `Think`, re-checkpoints, and resets so Firecracker exits cleanly in ~2 s. (`crates/guest-runner`; cognition `remote` feature gated off for the offline build.) |
| **F2b** ✅ | swap the channel to **vsock**: runner serves `health` / `checkpoint` / `shutdown`; host sends the `Package` and pulls a checkpoint back. **Done — validated in-VM**: guest listens on `AF_VSOCK` (raw libc); host drives deploy → health → checkpoint → shutdown over Firecracker's vsock UDS. The host sends a fresh agent (budget 100), the guest runs a `Think` (→ 95), and both `health` and the pulled-back checkpoint report 95 — **guest-level state continuity proven**, closing the F2a gap. Shutdown over vsock resets the VM. |
| **F3** ✅ | host-side `FirecrackerDeploy` (feature `firecracker`, pure std) + `MicroVm` handle. **Done — validated on the KVM host**: the Rust API spawns Firecracker, configures kernel/rootfs/vsock over a hand-rolled HTTP-over-UDS client, starts the VM, then drives the agent over vsock — `deploy` → `health` → `checkpoint` → `shutdown` — with `Drop` teardown. Run end-to-end via the musl `thaliox-runner fc-launch` (no Rust on the host); agent budget 100 → 95, checkpoint pulled back, VM reset, no leftover processes. (`runtime::firecracker`, `runtime::vmproto` shared with the guest.) Note: `FirecrackerDeploy` returns a `MicroVm` (the agent runs in-VM), so it is a sibling of `DeployTarget` rather than an impl of it — the unification is the `Package` deploy-unit + the vsock control surface, not the `-> Agent` signature. |
| **F4** ✅ | fast hibernate/resume via Firecracker VM snapshot on the `MicroVm` handle (perf layer). **Done — validated on the KVM host**: `snapshot` (pause + Full snapshot → memory + state files) and `FirecrackerDeploy::restore` (load into a fresh Firecracker + resume). The original VM is killed and the agent restored on a *new* process — `health` reports budget 95 with **no re-deploy**: the agent's live in-RAM state survived the VM snapshot/restore. (`thaliox-runner fc-snapshot`.) |

Each stage is independently demonstrable on the §10 host.

---

## 10. Appendix — host provisioning runbook (validated 2026-06-08)

> **Re-provision:** the executable one-shot provisioning script lives in the **private**
> `thaliox-process` repo (`recovery/fc-host-setup.sh`) — it performs everything below
> (KVM precheck → packages → pinned firecracker v1.16 + kernel 5.10.245 + ubuntu-24.04
> rootfs → ext4). The Firecracker host is **opportunistic**: a KVM box can be released when
> idle and re-provisioned on demand — nothing is lost (the runner is rebuilt from source,
> kernel/rootfs re-downloaded). This appendix is the **public validation record**; the
> acceptance checklist is the §9 F3/F4 commands (`fc-launch` / `fc-snapshot`).

Host: bare-metal, x86_64, `/dev/kvm` present (Intel VT-x), 96 cores / 375 GiB, Debian 11 (kernel 5.10), **3 TB NVMe at `/mnt/data`** — put all VM images, snapshots, and build artifacts there. github + S3 reachable directly (no mirror).

```sh
# work on the NVMe data disk
mkdir -p /mnt/data/firecracker && cd /mnt/data/firecracker
ARCH=x86_64; FC=v1.16.0
# firecracker + jailer
curl -fSL https://github.com/firecracker-microvm/firecracker/releases/download/$FC/firecracker-$FC-$ARCH.tgz | tar -xz
cp release-$FC-$ARCH/firecracker-$FC-$ARCH firecracker && cp release-$FC-$ARCH/jailer-$FC-$ARCH jailer && chmod +x firecracker jailer
# guest kernel + rootfs (Firecracker CI artifacts)
S3=https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.15/x86_64
curl -fSL $S3/vmlinux-5.10.245 -o vmlinux
curl -fSL $S3/ubuntu-24.04.squashfs -o rootfs.squashfs
# squashfs (read-only) -> writable ext4
apt-get install -y squashfs-tools
unsquashfs -d squashfs-root rootfs.squashfs
truncate -s 1G rootfs.ext4 && mkfs.ext4 -F -d squashfs-root rootfs.ext4
```

**Boot** (API socket): `PUT /boot-source` (`boot_args="console=ttyS0 reboot=k panic=1 pci=off random.trust_cpu=on root=/dev/vda rw"`), `PUT /drives/rootfs`, `PUT /machine-config` (`vcpu_count=2, mem_size_mib=512`), `PUT /actions InstanceStart`.

**Snapshot/restore** (validated): `PATCH /vm {"state":"Paused"}` → `PUT /snapshot/create` (Full → `memory.file` ≈ 512 MiB + `snapshot.file`) → fresh `firecracker` → `PUT /snapshot/load` (`mem_backend File`, `resume_vm:true`) → restored VM reports `state: Running`. **All steps returned 204; the guest booted Ubuntu 24.04 to a login prompt and the restored VMM resumed.**

> Honest gap: validation is VMM-level (resume to `Running`). Guest-level *state continuity* (inject a marker pre-snapshot, observe it post-restore) is proven once the runner's vsock channel exists (F2b).

---

## 11. Open questions

1. ~~vsock framing for the `Package`~~ — **resolved (F2b)**: a tiny typed protocol `[op: u8][len: u64 LE][payload]`, one request/response per connection (`Deploy` / `Health` / `Checkpoint` / `Shutdown`).
2. rootfs strategy — one shared read-only base + per-agent overlay/CoW, vs a built image per deploy? (CoW wins for density; needs an overlay-init in the guest.)
3. `jailer` from day one, or bare `firecracker` until F3 hardening?
4. how does a Firecracker VM snapshot (F4) coexist with the agent checkpoint when both exist — precedence on resume?
5. networking: does the agent need egress (tools / cognition) from inside the VM, and if so tap + NAT vs a host-side proxy over vsock?

---

## 12. Conclusion

The microVM leg adds no new data model — `Package` and `DeployTarget` already carry it. It adds a **guest runner** and a **host launcher**, joined by a **vsock** control channel, with the **portable agent checkpoint kept canonical** and Firecracker's VM snapshot demoted to a performance layer. The host chain is already proven on real hardware (§10); what remains is code, staged F2→F4, self-hosted and feature-gated so the software layer and its CI gate stay untouched.
