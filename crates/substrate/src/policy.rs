//! The **INV-2 deny-floor compiler** — capability scope, enforced below userland.
//!
//! RFC-0008 §3b: each agent's held capabilities are compiled into a deny-only
//! seccomp profile attached to the agent's process. The runtime check stays
//! authoritative for *grants* (it knows semantics the syscall layer cannot);
//! the floor only guarantees that an agent whose tokens warrant no network can
//! not *make* a connect syscall in the first place — forging past the runtime
//! now also means forging past the kernel.
//!
//! Per INV-5, the floor is generated from capability state the control plane
//! already governs: no human writes filter rules, and a capability change
//! recompiles the floor.

use std::collections::BTreeSet;
use std::fmt;

use thaliox_core::{AgentId, CapabilityToken, Permission, ResourceKind};

/// Coarse syscall families the floor reasons in. Deliberately coarse: the
/// deny-floor is defense-in-depth, not the authority — precision lives in the
/// runtime check (and pushing full scope semantics into BPF is an open
/// question of RFC-0008 §10.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SyscallGroup {
    /// Socket creation / connect / accept / send / recv.
    Network,
    /// Mutating filesystem state (open-for-write, unlink, rename, chmod, …).
    FilesystemWrite,
    /// Spawning processes (fork / execve / clone-as-process).
    ProcessSpawn,
    /// Inspecting or manipulating other processes (ptrace, process_vm_*).
    Ptrace,
    /// Loading kernel modules / raw BPF.
    ModuleLoad,
    /// Mount / namespace surgery.
    MountNs,
}

impl SyscallGroup {
    pub const ALL: [SyscallGroup; 6] = [
        SyscallGroup::Network,
        SyscallGroup::FilesystemWrite,
        SyscallGroup::ProcessSpawn,
        SyscallGroup::Ptrace,
        SyscallGroup::ModuleLoad,
        SyscallGroup::MountNs,
    ];

    /// The syscalls the group denies (x86-64/aarch64 shared names; the live
    /// leg resolves per-arch numbers at load time).
    pub fn syscalls(self) -> &'static [&'static str] {
        match self {
            SyscallGroup::Network => &[
                "socket", "connect", "accept", "accept4", "bind", "listen", "sendto", "recvfrom",
                "sendmsg", "recvmsg",
            ],
            SyscallGroup::FilesystemWrite => &[
                "unlink",
                "unlinkat",
                "rename",
                "renameat",
                "mkdir",
                "mkdirat",
                "rmdir",
                "chmod",
                "fchmod",
                "chown",
                "fchown",
                "truncate",
                "ftruncate",
            ],
            SyscallGroup::ProcessSpawn => &["fork", "vfork", "execve", "execveat"],
            SyscallGroup::Ptrace => &["ptrace", "process_vm_readv", "process_vm_writev"],
            SyscallGroup::ModuleLoad => &["init_module", "finit_module", "delete_module", "bpf"],
            SyscallGroup::MountNs => &["mount", "umount2", "unshare", "setns", "pivot_root"],
        }
    }

    /// Whether a held `(permission, resource)` scope warrants this group.
    /// `Admin` implies every permission (INV-5: the control plane's class), so
    /// it warrants everything.
    fn warranted_by(self, permission: Permission, resource: ResourceKind) -> bool {
        if permission == Permission::Admin {
            return true;
        }
        let any = resource == ResourceKind::Any;
        match self {
            // Tools do network I/O; Communicate is network by definition.
            SyscallGroup::Network => {
                permission == Permission::Communicate
                    || (permission == Permission::Execute
                        && (resource == ResourceKind::Tool || any))
            }
            // Persistent writes are warranted by write scope over stateful kinds.
            SyscallGroup::FilesystemWrite => {
                permission == Permission::Write
                    && (resource == ResourceKind::Memory || resource == ResourceKind::Space || any)
            }
            SyscallGroup::ProcessSpawn => permission == Permission::Spawn,
            // The high-risk tail is warranted by nothing short of Admin.
            SyscallGroup::Ptrace | SyscallGroup::ModuleLoad | SyscallGroup::MountNs => false,
        }
    }
}

impl fmt::Display for SyscallGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// The compiled deny-only floor for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyFloor {
    pub agent: AgentId,
    pub denied: BTreeSet<SyscallGroup>,
}

impl DenyFloor {
    /// Render as an OCI-style seccomp profile (the format Firecracker's
    /// jailer and container runtimes consume): default-allow, with every
    /// denied group's syscalls returning `EPERM`. Deterministic output
    /// (groups and names in stable order).
    pub fn to_oci_json(&self) -> String {
        let names: Vec<&str> = self
            .denied
            .iter()
            .flat_map(|g| g.syscalls().iter().copied())
            .collect();
        let profile = serde_json::json!({
            "defaultAction": "SCMP_ACT_ALLOW",
            "syscalls": if names.is_empty() {
                serde_json::json!([])
            } else {
                serde_json::json!([{
                    "names": names,
                    "action": "SCMP_ACT_ERRNO",
                    "errnoRet": 1
                }])
            },
        });
        serde_json::to_string_pretty(&profile).expect("static structure serializes")
    }
}

/// Compile an agent's held capabilities into its deny floor: every group no
/// held scope warrants is denied. No capabilities ⇒ everything denied; an
/// `Admin` grant ⇒ nothing denied (the governor's class implies all).
pub fn compile_deny_floor(agent: &AgentId, caps: &[CapabilityToken]) -> DenyFloor {
    let denied = SyscallGroup::ALL
        .into_iter()
        .filter(|group| {
            !caps.iter().any(|cap| {
                cap.permissions
                    .iter()
                    .any(|&p| cap.scope.iter().any(|s| group.warranted_by(p, s.resource)))
            })
        })
        .collect();
    DenyFloor {
        agent: agent.clone(),
        denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thaliox_core::Scope;

    fn cap(permissions: Vec<Permission>, resource: ResourceKind) -> CapabilityToken {
        CapabilityToken {
            subject: AgentId::new("a1"),
            permissions,
            scope: vec![Scope {
                resource,
                pattern: "*".into(),
            }],
            issued_at: 0,
            expires_at: 0,
            jti: [0; 16],
            delegable: false,
            signature: [0; 32],
        }
    }

    fn floor(caps: &[CapabilityToken]) -> DenyFloor {
        compile_deny_floor(&AgentId::new("a1"), caps)
    }

    #[test]
    fn no_capabilities_means_everything_denied() {
        let f = floor(&[]);
        assert_eq!(f.denied.len(), SyscallGroup::ALL.len());
    }

    #[test]
    fn tool_execute_warrants_network_and_nothing_else() {
        let f = floor(&[cap(vec![Permission::Execute], ResourceKind::Tool)]);
        assert!(!f.denied.contains(&SyscallGroup::Network));
        assert!(f.denied.contains(&SyscallGroup::FilesystemWrite));
        assert!(f.denied.contains(&SyscallGroup::ProcessSpawn));
        assert!(f.denied.contains(&SyscallGroup::Ptrace));
    }

    #[test]
    fn memory_write_warrants_filesystem_but_not_network() {
        let f = floor(&[cap(vec![Permission::Write], ResourceKind::Memory)]);
        assert!(!f.denied.contains(&SyscallGroup::FilesystemWrite));
        assert!(f.denied.contains(&SyscallGroup::Network));
    }

    #[test]
    fn spawn_warrants_process_spawn() {
        let f = floor(&[cap(vec![Permission::Spawn], ResourceKind::Agent)]);
        assert!(!f.denied.contains(&SyscallGroup::ProcessSpawn));
        assert!(f.denied.contains(&SyscallGroup::Network));
    }

    #[test]
    fn admin_warrants_everything() {
        // INV-5: Admin implies all — the governor's floor is empty.
        let f = floor(&[cap(vec![Permission::Admin], ResourceKind::Agent)]);
        assert!(f.denied.is_empty());
    }

    #[test]
    fn the_high_risk_tail_needs_admin() {
        // Even a maximally-scoped non-admin grant never warrants ptrace,
        // module loading, or mount/namespace surgery.
        let f = floor(&[cap(
            vec![
                Permission::Read,
                Permission::Write,
                Permission::Execute,
                Permission::Spawn,
                Permission::Communicate,
            ],
            ResourceKind::Any,
        )]);
        assert!(f.denied.contains(&SyscallGroup::Ptrace));
        assert!(f.denied.contains(&SyscallGroup::ModuleLoad));
        assert!(f.denied.contains(&SyscallGroup::MountNs));
        assert!(!f.denied.contains(&SyscallGroup::Network));
    }

    #[test]
    fn more_capability_never_means_more_denial() {
        // Monotonicity: adding a grant can only shrink the deny set.
        let small = floor(&[cap(vec![Permission::Execute], ResourceKind::Tool)]);
        let big = floor(&[
            cap(vec![Permission::Execute], ResourceKind::Tool),
            cap(vec![Permission::Write], ResourceKind::Memory),
            cap(vec![Permission::Spawn], ResourceKind::Agent),
        ]);
        assert!(big.denied.is_subset(&small.denied));
    }

    #[test]
    fn oci_profile_is_deterministic_and_deny_only() {
        let f = floor(&[cap(vec![Permission::Execute], ResourceKind::Tool)]);
        let a = f.to_oci_json();
        let b = f.to_oci_json();
        assert_eq!(a, b);
        assert!(a.contains("\"defaultAction\": \"SCMP_ACT_ALLOW\""));
        assert!(a.contains("SCMP_ACT_ERRNO"));
        assert!(a.contains("\"ptrace\""));
        assert!(!a.contains("\"socket\"")); // network is warranted → not denied

        // The empty floor (Admin) renders as pure default-allow.
        let admin = floor(&[cap(vec![Permission::Admin], ResourceKind::Any)]);
        let j = admin.to_oci_json();
        assert!(j.contains("\"syscalls\": []"));
    }
}
