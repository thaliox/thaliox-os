//! Primitive #3: the **capability token** — permission & trust, replacing
//! uid/gid. (TAM §5)
//!
//! Two rules are mandatory and come directly from reviewing the earlier
//! prototype's bugs:
//!
//! 1. **Scope MUST be enforced.** Authorization checks both that `permissions`
//!    contains the required class *and* that some [`Scope`] matches the target.
//!    Checking the permission class alone (ignoring scope) is non-conformant.
//! 2. **Signed payloads MUST be canonically, unambiguously encoded** —
//!    length-prefixed or canonical CBOR, never delimiter-joined (`|` / `,`),
//!    to rule out delimiter-injection forgery. (The signing lives in
//!    `thaliox-cap`; this crate defines the contract and the scope check.)

use serde::{Deserialize, Serialize};

use crate::agent::AgentId;

/// An unforgeable grant: what an agent may do, and over what scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityToken {
    /// The holder.
    pub subject: AgentId,
    /// Permitted operation classes.
    pub permissions: Vec<Permission>,
    /// Scopes the grant applies to — **must be enforced** on every check.
    pub scope: Vec<Scope>,
    pub issued_at: u64,
    /// Expiry (unix seconds); `0` = never.
    pub expires_at: u64,
    /// Unique id, for revocation and replay protection.
    pub jti: [u8; 16],
    /// Whether this token may be delegated to a sub-agent.
    pub delegable: bool,
    /// Signature over the canonically-encoded payload (see `thaliox-cap`).
    pub signature: [u8; 32],
}

/// Operation classes a capability may grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    Read,
    Write,
    Execute,
    Spawn,
    Communicate,
    /// Implies all permissions except `Sovereign`.
    Admin,
    /// INV-5: the human-only supreme capability. Never delegable.
    Sovereign,
}

impl Permission {
    /// Whether holding `self` satisfies a requirement for `needed`.
    /// `Admin` implies everything except `Sovereign`; `Sovereign` implies only itself.
    pub fn implies(self, needed: Permission) -> bool {
        if self == needed {
            return true;
        }
        matches!(self, Permission::Admin) && needed != Permission::Sovereign
    }
}

/// What kind of resource a scope addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceKind {
    Memory,
    Agent,
    Tool,
    Space,
    Model,
    /// Matches any resource kind.
    Any,
}

/// A scope: a resource kind plus a glob pattern over targets, e.g.
/// `{ resource: Memory, pattern: "mem://team-a/*" }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    pub resource: ResourceKind,
    pub pattern: String,
}

impl Scope {
    /// **INV-2 rule 1**: a scope matches only if the resource kind fits AND the
    /// glob pattern matches `target`.
    pub fn matches(&self, resource: ResourceKind, target: &str) -> bool {
        (self.resource == ResourceKind::Any || self.resource == resource)
            && glob_match(&self.pattern, target)
    }
}

impl CapabilityToken {
    /// **INV-2**: does this token authorize `permission` over `(resource, target)`?
    /// Checks the permission class **and** scope — never one without the other.
    pub fn authorizes(&self, permission: Permission, resource: ResourceKind, target: &str) -> bool {
        let has_perm = self.permissions.iter().any(|p| p.implies(permission));
        let in_scope = self.scope.iter().any(|s| s.matches(resource, target));
        has_perm && in_scope
    }
}

/// Minimal wildcard matcher: `*` matches any (possibly empty) run of characters.
/// A full path-glob matcher arrives with the `thaliox-cap` implementation; this
/// is the contract's reference semantics.
fn glob_match(pattern: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), s.chars().collect());
    // Classic two-pointer wildcard match with backtracking on `*`.
    let (mut pi, mut ti, mut star, mut mark) = (0usize, 0usize, None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_implies_all_but_sovereign() {
        assert!(Permission::Admin.implies(Permission::Write));
        assert!(Permission::Admin.implies(Permission::Spawn));
        assert!(!Permission::Admin.implies(Permission::Sovereign));
        assert!(Permission::Read.implies(Permission::Read));
        assert!(!Permission::Read.implies(Permission::Write));
    }

    #[test]
    fn scope_is_enforced_not_just_permission() {
        let tok = CapabilityToken {
            subject: AgentId::new("thaliox://a/x"),
            permissions: vec![Permission::Write],
            scope: vec![Scope {
                resource: ResourceKind::Memory,
                pattern: "mem://team-a/*".into(),
            }],
            issued_at: 0,
            expires_at: 0,
            jti: [0; 16],
            delegable: false,
            signature: [0; 32],
        };
        // Right permission AND in scope → authorized.
        assert!(tok.authorizes(
            Permission::Write,
            ResourceKind::Memory,
            "mem://team-a/notes"
        ));
        // Right permission but OUT of scope → denied (the bug INV-2 rule 1 forbids).
        assert!(!tok.authorizes(
            Permission::Write,
            ResourceKind::Memory,
            "mem://team-b/secret"
        ));
        // Wrong resource kind → denied.
        assert!(!tok.authorizes(Permission::Write, ResourceKind::Tool, "mem://team-a/notes"));
    }

    #[test]
    fn glob_matches() {
        assert!(glob_match("mem://team-a/*", "mem://team-a/x/y"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*c", "abbbc"));
        assert!(!glob_match("a*c", "abbb"));
    }
}
