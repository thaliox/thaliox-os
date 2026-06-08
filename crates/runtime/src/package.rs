//! # Packaging & one-click deployment (M2)
//!
//! The first leg of M2 (MASTER_PLAN §6): *one-click deployment*. A [`Package`]
//! is the portable, self-describing **deployment unit** — a [`Manifest`] of
//! what the agent needs plus its [`Checkpoint`] (the portable state). It
//! serializes to bytes, so it is one artifact you can ship and launch anywhere.
//!
//! [`DeployTarget`] is the launcher interface — *fix the interface, not the
//! mechanism* (TAM §4.2). [`LocalDeploy`] is the in-process software
//! realization of the microVM boundary: it validates the manifest against the
//! host-provided [`DeployEnv`] and [`restore`](crate::Agent::restore)s the agent
//! in the current process. A Firecracker target (booting a VM from the same
//! package) will implement the *same* trait once a KVM-capable host exists — the
//! package format and validation are unchanged, only the launch is.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thaliox_cognition::LlmProvider;
use thaliox_core::{AgentId, CapabilityVerifier, SemanticSpace, Tool};

use crate::{Agent, Checkpoint};

/// The package format version this build understands.
pub const PACKAGE_FORMAT: u32 = 1;

/// A self-describing record of what an agent needs to run, shipped alongside its
/// checkpoint. The host binds the concrete environment; the manifest is the
/// contract the binding is checked against at deploy time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// Package format version (see [`PACKAGE_FORMAT`]).
    pub format: u32,
    pub agent: AgentId,
    /// Expected mind identity ([`LlmProvider::id`]); empty = accept any.
    pub model_fingerprint: String,
    /// Memory namespace the agent expects (informational; the host binds the
    /// concrete [`SemanticSpace`]).
    pub memory_namespace: String,
    /// Tool names that MUST be bound at deploy time.
    pub required_tools: Vec<String>,
    pub note: String,
}

impl Manifest {
    /// A minimal manifest for `agent` at the current format version.
    pub fn new(agent: AgentId) -> Self {
        Self {
            format: PACKAGE_FORMAT,
            agent,
            model_fingerprint: String::new(),
            memory_namespace: String::new(),
            required_tools: Vec::new(),
            note: String::new(),
        }
    }
    /// Require a specific mind identity (builder-style).
    pub fn expecting_model(mut self, fingerprint: impl Into<String>) -> Self {
        self.model_fingerprint = fingerprint.into();
        self
    }
    /// Require a tool to be bound at deploy time (builder-style).
    pub fn requiring_tool(mut self, name: impl Into<String>) -> Self {
        self.required_tools.push(name.into());
        self
    }
}

/// A portable deployment unit: a manifest + the agent's checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub manifest: Manifest,
    pub checkpoint: Checkpoint,
}

impl Package {
    /// Bundle a running `agent` with a `manifest` into a deployable package.
    pub fn pack(agent: &Agent, manifest: Manifest) -> Self {
        Self {
            manifest,
            checkpoint: agent.checkpoint(),
        }
    }

    /// Serialize to the shippable byte artifact.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a Package is always serializable")
    }

    /// Parse a package, rejecting an unsupported format up front.
    pub fn from_bytes(blob: &[u8]) -> Result<Self, PackageError> {
        let pkg: Package =
            serde_json::from_slice(blob).map_err(|e| PackageError::Decode(e.to_string()))?;
        if pkg.manifest.format != PACKAGE_FORMAT {
            return Err(PackageError::UnsupportedFormat {
                found: pkg.manifest.format,
                supported: PACKAGE_FORMAT,
            });
        }
        Ok(pkg)
    }

    /// Frame the package for a one-way **config-drive** (RFC-0004 §4, F2a): an
    /// 8-byte little-endian length prefix followed by the JSON body. Written to a
    /// raw block image on the host, read back from `/dev/vdb` in the guest — the
    /// length prefix lets the reader ignore the drive's trailing zero padding.
    pub fn to_config_drive(&self) -> Vec<u8> {
        let body = self.to_bytes();
        let mut out = (body.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(&body);
        out
    }

    /// Parse a config-drive frame (length-prefixed) from raw device/file bytes.
    pub fn from_config_drive(bytes: &[u8]) -> Result<Self, PackageError> {
        let len_buf: [u8; 8] = bytes
            .get(..8)
            .ok_or_else(|| PackageError::Decode("config-drive shorter than 8 bytes".into()))?
            .try_into()
            .expect("slice of len 8");
        let len = u64::from_le_bytes(len_buf) as usize;
        let body = bytes
            .get(8..8 + len)
            .ok_or_else(|| PackageError::Decode("config-drive truncated".into()))?;
        Package::from_bytes(body)
    }
}

/// The concrete environment a host binds when launching a package.
pub struct DeployEnv {
    pub memory: Arc<dyn SemanticSpace>,
    pub mind: Arc<dyn LlmProvider>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub verifier: Option<Arc<dyn CapabilityVerifier>>,
}

/// Why a package could not be parsed or deployed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    /// The package format is not understood by this build.
    UnsupportedFormat { found: u32, supported: u32 },
    /// The bound mind does not match the manifest's required fingerprint.
    ModelMismatch { want: String, have: String },
    /// A required tool was not bound in the [`DeployEnv`].
    MissingTool(String),
    /// The checkpoint could not be restored.
    Restore(String),
    /// The byte artifact could not be decoded.
    Decode(String),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageError::UnsupportedFormat { found, supported } => {
                write!(
                    f,
                    "unsupported package format {found} (this build: {supported})"
                )
            }
            PackageError::ModelMismatch { want, have } => {
                write!(
                    f,
                    "model mismatch: package wants `{want}`, host bound `{have}`"
                )
            }
            PackageError::MissingTool(t) => write!(f, "required tool not bound: `{t}`"),
            PackageError::Restore(why) => write!(f, "restore failed: {why}"),
            PackageError::Decode(why) => write!(f, "package decode failed: {why}"),
        }
    }
}

impl Error for PackageError {}

/// Validate the host-bound environment against the manifest's requirements.
fn validate(manifest: &Manifest, env: &DeployEnv) -> Result<(), PackageError> {
    if manifest.format != PACKAGE_FORMAT {
        return Err(PackageError::UnsupportedFormat {
            found: manifest.format,
            supported: PACKAGE_FORMAT,
        });
    }
    if !manifest.model_fingerprint.is_empty() && manifest.model_fingerprint != env.mind.id() {
        return Err(PackageError::ModelMismatch {
            want: manifest.model_fingerprint.clone(),
            have: env.mind.id().to_string(),
        });
    }
    let bound: HashSet<&str> = env.tools.iter().map(|t| t.name()).collect();
    for needed in &manifest.required_tools {
        if !bound.contains(needed.as_str()) {
            return Err(PackageError::MissingTool(needed.clone()));
        }
    }
    Ok(())
}

/// A launcher: turns a [`Package`] into a running [`Agent`].
pub trait DeployTarget {
    /// Launch `package`, binding the host-provided `env`.
    fn deploy(&self, package: &Package, env: DeployEnv) -> Result<Agent, PackageError>;
}

/// In-process deployment — the software realization of the microVM boundary.
/// (A Firecracker target will implement [`DeployTarget`] identically, differing
/// only in *where* the restored agent runs.)
pub struct LocalDeploy;

impl DeployTarget for LocalDeploy {
    fn deploy(&self, package: &Package, env: DeployEnv) -> Result<Agent, PackageError> {
        validate(&package.manifest, &env)?;
        let mut agent = Agent::restore(&package.checkpoint, env.memory, env.mind)
            .map_err(|e| PackageError::Restore(e.to_string()))?;
        for tool in env.tools {
            agent = agent.with_tool(tool);
        }
        if let Some(v) = env.verifier {
            agent = agent.with_verifier(v);
        }
        Ok(agent)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use thaliox_cognition::MockProvider;
    use thaliox_core::{AgentId, AttentionBudget, TamError, ToolResult};
    use thaliox_memory::InMemorySpace;

    use super::*;

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        async fn invoke(&self, input: &str) -> Result<ToolResult, TamError> {
            Ok(ToolResult {
                output: input.to_string(),
                cost: 1,
            })
        }
    }

    fn agent() -> Agent {
        let mut a = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(100, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        );
        a.start().unwrap();
        a
    }

    fn env_with_echo() -> DeployEnv {
        DeployEnv {
            memory: Arc::new(InMemorySpace::new()),
            mind: Arc::new(MockProvider::new("ok", 5)),
            tools: vec![Arc::new(EchoTool)],
            verifier: None,
        }
    }

    #[test]
    fn pack_round_trips_through_bytes() {
        let pkg = Package::pack(&agent(), Manifest::new(AgentId::new("a1")));
        let restored = Package::from_bytes(&pkg.to_bytes()).unwrap();
        assert_eq!(restored.manifest, pkg.manifest);
        assert_eq!(restored.checkpoint.state, pkg.checkpoint.state);
    }

    #[test]
    fn config_drive_round_trips_with_padding() {
        let pkg = Package::pack(&agent(), Manifest::new(AgentId::new("a1")));
        // Simulate a raw block device: frame + trailing zero padding.
        let mut drive = pkg.to_config_drive();
        drive.resize(drive.len() + 4096, 0);
        let restored = Package::from_config_drive(&drive).unwrap();
        assert_eq!(restored.checkpoint.state, pkg.checkpoint.state);
        assert_eq!(restored.manifest, pkg.manifest);
    }

    #[test]
    fn from_config_drive_rejects_short_input() {
        assert!(matches!(
            Package::from_config_drive(&[0, 1, 2]),
            Err(PackageError::Decode(_))
        ));
    }

    #[test]
    fn local_deploy_restores_a_running_agent() {
        // One-click: pack → ship bytes → deploy on a fresh host.
        let manifest = Manifest::new(AgentId::new("a1"))
            .expecting_model("local-mock")
            .requiring_tool("echo");
        let bytes = Package::pack(&agent(), manifest).to_bytes();

        let pkg = Package::from_bytes(&bytes).unwrap();
        let deployed = LocalDeploy.deploy(&pkg, env_with_echo()).unwrap();

        assert_eq!(deployed.id(), &AgentId::new("a1"));
        assert_eq!(deployed.remaining_budget(), 100);
        assert_eq!(deployed.phase(), crate::Phase::Live);
    }

    #[test]
    fn deploy_rejects_missing_tool() {
        let manifest = Manifest::new(AgentId::new("a1")).requiring_tool("echo");
        let pkg = Package::pack(&agent(), manifest);
        let env = DeployEnv {
            tools: vec![], // echo not bound
            ..env_with_echo()
        };
        assert_eq!(
            LocalDeploy.deploy(&pkg, env).err(),
            Some(PackageError::MissingTool("echo".into()))
        );
    }

    #[test]
    fn deploy_rejects_model_mismatch() {
        let manifest = Manifest::new(AgentId::new("a1")).expecting_model("anthropic");
        let pkg = Package::pack(&agent(), manifest);
        assert_eq!(
            LocalDeploy.deploy(&pkg, env_with_echo()).err(),
            Some(PackageError::ModelMismatch {
                want: "anthropic".into(),
                have: "local-mock".into(),
            })
        );
    }

    #[test]
    fn from_bytes_rejects_unsupported_format() {
        let mut pkg = Package::pack(&agent(), Manifest::new(AgentId::new("a1")));
        pkg.manifest.format = 999;
        assert_eq!(
            Package::from_bytes(&pkg.to_bytes()).err(),
            Some(PackageError::UnsupportedFormat {
                found: 999,
                supported: PACKAGE_FORMAT,
            })
        );
    }

    #[test]
    fn from_bytes_rejects_garbage() {
        assert!(matches!(
            Package::from_bytes(b"not a package"),
            Err(PackageError::Decode(_))
        ));
    }
}
