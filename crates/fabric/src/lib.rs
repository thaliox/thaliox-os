//! # THALIOX fabric (L3)
//!
//! agent↔agent communication, team orchestration, and CRDT state replication
//! (MASTER_PLAN §2). Transport carries [`VectorMessage`]s (TAM §3) near-term
//! over gRPC/QUIC, long-term the `vsend`/`vrecv` hardware
//! primitive. A **team** is a *holarchy* — agents that are whole yet compose
//! into a larger whole.
//!
//! M4a: a real in-process [`LocalFabric`] routes [`VectorMessage`]s between
//! [`Endpoint`]s, enforcing INV-2 (capability-gated) on send and INV-3 (vector
//! fidelity, via [`fidelity`]) on injection.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use thaliox_core::{
    AgentId, ModelFingerprint, Permission, Recipient, ResourceKind, TamError, VectorMessage,
    VectorPayload,
};
use thaliox_runtime::{
    Checkpoint, DeployEnv, DeployTarget, LocalDeploy, Node, NodeId, Package, Supervisor,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Carries vector messages between agents.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a vector message (unicast or multicast to an intent group).
    async fn send(&self, msg: VectorMessage) -> Result<(), TamError>;

    /// Receive the next inbound vector message.
    async fn recv(&self) -> Result<VectorMessage, TamError>;
}

/// Composable collaboration paradigms a team may adopt (MASTER_PLAN §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paradigm {
    Hierarchy,
    Market,
    Swarm,
    Pipeline,
}

/// A team: a holarchy of agents with shared goals and assigned roles.
#[derive(Debug, Clone)]
pub struct Team {
    pub name: String,
    pub members: Vec<AgentId>,
    pub paradigm: Paradigm,
}

// ---------- M4a: in-process fabric (RFC-0006 §2) ----------

/// A shared, in-process message fabric. Routes [`VectorMessage`]s between agent
/// [`Endpoint`]s by recipient (unicast or multicast intent group). Cloneable —
/// every endpoint shares the same routing state.
#[derive(Clone, Default)]
pub struct LocalFabric {
    inboxes: Arc<Mutex<HashMap<AgentId, VecDeque<VectorMessage>>>>,
    groups: Arc<Mutex<HashMap<String, HashSet<AgentId>>>>,
}

impl LocalFabric {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent and get its endpoint. `fingerprint` is the agent's
    /// vector space, used for INV-3 fidelity checks on what it receives.
    pub fn endpoint(&self, id: AgentId, fingerprint: ModelFingerprint) -> Endpoint {
        self.inboxes.lock().unwrap().entry(id.clone()).or_default();
        Endpoint {
            id,
            fingerprint,
            fabric: self.clone(),
        }
    }

    /// Add an agent to a multicast intent group.
    pub fn join(&self, id: &AgentId, group: &str) {
        self.groups
            .lock()
            .unwrap()
            .entry(group.to_string())
            .or_default()
            .insert(id.clone());
    }

    fn deliver(&self, to: &AgentId, msg: VectorMessage) {
        self.inboxes
            .lock()
            .unwrap()
            .entry(to.clone())
            .or_default()
            .push_back(msg);
    }

    fn group_members(&self, group: &str) -> Vec<AgentId> {
        self.groups
            .lock()
            .unwrap()
            .get(group)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// One agent's handle on the fabric.
pub struct Endpoint {
    id: AgentId,
    fingerprint: ModelFingerprint,
    fabric: LocalFabric,
}

impl Endpoint {
    pub fn id(&self) -> &AgentId {
        &self.id
    }
    pub fn fingerprint(&self) -> &ModelFingerprint {
        &self.fingerprint
    }
}

#[async_trait]
impl Transport for Endpoint {
    async fn send(&self, msg: VectorMessage) -> Result<(), TamError> {
        authorize_communicate(&msg)?;
        match msg.to.clone() {
            Recipient::Unicast(id) => self.fabric.deliver(&id, msg),
            Recipient::Multicast(g) => {
                for m in self.fabric.group_members(&g.0) {
                    self.fabric.deliver(&m, msg.clone());
                }
            }
        }
        Ok(())
    }

    async fn recv(&self) -> Result<VectorMessage, TamError> {
        self.fabric
            .inboxes
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .and_then(|q| q.pop_front())
            .ok_or_else(|| TamError::Invalid(format!("no message for {}", self.id)))
    }
}

/// INV-3 verdict: whether a received message can be injected into the receiver's
/// model directly, must be translated first, or is an unaligned escape hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fidelity {
    /// Same fingerprint, aligned vector — inject directly, zero loss.
    Lossless,
    /// Different fingerprint, aligned vector — MUST be explicitly translated.
    NeedsTranslation,
    /// `Raw` payload — unaligned interop; no zero-loss guarantee.
    Unaligned,
}

/// **INV-3 (vector fidelity)**: classify `msg` for a receiver with `receiver`'s
/// fingerprint. Forbids *implicit* lossy cross-fingerprint injection — a
/// mismatched aligned payload is `NeedsTranslation`, never silently `Lossless`.
pub fn fidelity(msg: &VectorMessage, receiver: &ModelFingerprint) -> Fidelity {
    match &msg.payload {
        VectorPayload::Raw { .. } => Fidelity::Unaligned,
        _ if msg.fingerprint.compatible_with(receiver) => Fidelity::Lossless,
        _ => Fidelity::NeedsTranslation,
    }
}

/// INV-2: a cross-agent send must carry a capability granting `Communicate` over
/// the recipient (an agent id or an intent group). Shared by the local and
/// networked transports.
fn authorize_communicate(msg: &VectorMessage) -> Result<(), TamError> {
    let target = match &msg.to {
        Recipient::Unicast(id) => id.0.as_str(),
        Recipient::Multicast(g) => g.0.as_str(),
    };
    let ok = msg
        .capability
        .as_ref()
        .is_some_and(|c| c.authorizes(Permission::Communicate, ResourceKind::Agent, target));
    if ok {
        Ok(())
    } else {
        Err(TamError::CapabilityDenied(format!(
            "Communicate on {target}"
        )))
    }
}

// ---------- M4b: networked fabric over TCP (RFC-0006 §2-3) ----------

fn io_err(e: std::io::Error) -> TamError {
    TamError::Invalid(format!("net: {e}"))
}

/// A node in a networked cluster: local agents are routed in-process; remote
/// agents are reached over TCP. Same `VectorMessage` wire (length-prefixed serde),
/// so the flow is identical to [`LocalFabric`] — only the hop differs.
#[derive(Clone, Default)]
pub struct NetNode {
    local: LocalFabric,
    routes: Arc<Mutex<HashMap<AgentId, SocketAddr>>>,
}

impl NetNode {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a local agent and get its endpoint.
    pub fn endpoint(&self, id: AgentId, fingerprint: ModelFingerprint) -> NetEndpoint {
        self.local
            .inboxes
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_default();
        NetEndpoint {
            id,
            fingerprint,
            local: self.local.clone(),
            routes: self.routes.clone(),
        }
    }

    /// Record that a remote agent lives on the node listening at `addr`.
    pub fn route(&self, remote: AgentId, addr: SocketAddr) {
        self.routes.lock().unwrap().insert(remote, addr);
    }

    /// Bind a TCP listener and spawn an accept loop that delivers inbound
    /// `VectorMessage`s to local inboxes. Returns the bound address.
    pub async fn listen(&self, addr: SocketAddr) -> Result<SocketAddr, TamError> {
        let listener = TcpListener::bind(addr).await.map_err(io_err)?;
        let bound = listener.local_addr().map_err(io_err)?;
        let local = self.local.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    continue;
                };
                let mut len = [0u8; 4];
                if stream.read_exact(&mut len).await.is_err() {
                    continue;
                }
                let n = u32::from_le_bytes(len) as usize;
                let mut buf = vec![0u8; n];
                if stream.read_exact(&mut buf).await.is_err() {
                    continue;
                }
                if let Ok(msg) = serde_json::from_slice::<VectorMessage>(&buf)
                    && let Recipient::Unicast(id) = msg.to.clone()
                {
                    local.deliver(&id, msg);
                }
            }
        });
        Ok(bound)
    }
}

/// One agent's handle on a [`NetNode`]: sends locally or over TCP, receives from
/// its local inbox.
pub struct NetEndpoint {
    id: AgentId,
    fingerprint: ModelFingerprint,
    local: LocalFabric,
    routes: Arc<Mutex<HashMap<AgentId, SocketAddr>>>,
}

impl NetEndpoint {
    pub fn id(&self) -> &AgentId {
        &self.id
    }
    pub fn fingerprint(&self) -> &ModelFingerprint {
        &self.fingerprint
    }
}

#[async_trait]
impl Transport for NetEndpoint {
    async fn send(&self, msg: VectorMessage) -> Result<(), TamError> {
        authorize_communicate(&msg)?;
        let to = match &msg.to {
            Recipient::Unicast(id) => id.clone(),
            Recipient::Multicast(_) => {
                return Err(TamError::Invalid(
                    "multicast over the network is M4c".into(),
                ));
            }
        };
        // Local agent → in-process delivery. (Compute under the lock, release it,
        // then deliver — `deliver` re-locks.)
        let is_local = self.local.inboxes.lock().unwrap().contains_key(&to);
        if is_local {
            self.local.deliver(&to, msg);
            return Ok(());
        }
        // Remote agent → TCP to its node (length-prefixed serde).
        let addr = self
            .routes
            .lock()
            .unwrap()
            .get(&to)
            .copied()
            .ok_or_else(|| TamError::Invalid(format!("no route to {to}")))?;
        let bytes = serde_json::to_vec(&msg).map_err(|e| TamError::Invalid(e.to_string()))?;
        let mut stream = TcpStream::connect(addr).await.map_err(io_err)?;
        stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .await
            .map_err(io_err)?;
        stream.write_all(&bytes).await.map_err(io_err)?;
        stream.flush().await.map_err(io_err)?;
        Ok(())
    }

    async fn recv(&self) -> Result<VectorMessage, TamError> {
        self.local
            .inboxes
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .and_then(|q| q.pop_front())
            .ok_or_else(|| TamError::Invalid(format!("no message for {}", self.id)))
    }
}

// ---------- M4b: networked migration (RFC-0005 §3 over the fabric) ----------

/// Send an agent's `Package` to a remote node's migration listener over TCP, and
/// wait for it to be accepted. The cross-host arm of M3's `migrate` — same
/// `Package`, the bytes now crossing the network (RFC-0006 §3).
pub async fn send_migration(addr: SocketAddr, package: &Package) -> Result<(), TamError> {
    let bytes = package.to_bytes();
    let mut s = TcpStream::connect(addr).await.map_err(io_err)?;
    s.write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .map_err(io_err)?;
    s.write_all(&bytes).await.map_err(io_err)?;
    s.flush().await.map_err(io_err)?;
    let mut ack = [0u8; 1];
    s.read_exact(&mut ack).await.map_err(io_err)?;
    if ack[0] == 1 {
        Ok(())
    } else {
        Err(TamError::Invalid("remote rejected the migration".into()))
    }
}

/// Listen for inbound migrations: receive a `Package`, deploy it with a freshly
/// bound environment, and host it on `node`. `env` is a factory because each
/// deploy binds its own memory/mind (RFC-0002 §3.4). Returns the bound address.
pub async fn serve_migrations<E>(
    node: Arc<Mutex<Node>>,
    env: E,
    addr: SocketAddr,
) -> Result<SocketAddr, TamError>
where
    E: Fn() -> DeployEnv + Send + Sync + 'static,
{
    let listener = TcpListener::bind(addr).await.map_err(io_err)?;
    let bound = listener.local_addr().map_err(io_err)?;
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let mut len = [0u8; 4];
            if stream.read_exact(&mut len).await.is_err() {
                continue;
            }
            let n = u32::from_le_bytes(len) as usize;
            let mut buf = vec![0u8; n];
            if stream.read_exact(&mut buf).await.is_err() {
                continue;
            }
            let ok = match Package::from_bytes(&buf) {
                Ok(pkg) => match LocalDeploy.deploy(&pkg, env()) {
                    Ok(agent) => {
                        node.lock().unwrap().host(agent);
                        true
                    }
                    Err(_) => false,
                },
                Err(_) => false,
            };
            let _ = stream.write_all(&[ok as u8]).await;
        }
    });
    Ok(bound)
}

// ---------- M4b: distributed heartbeat (Supervisor over the fabric, RFC-0005 §5) ----------

#[derive(serde::Serialize, serde::Deserialize)]
struct Heartbeat {
    agent: AgentId,
    node: String,
    checkpoint: Checkpoint,
}

/// A node reports an agent's liveness + latest `Checkpoint` to the supervisor
/// over TCP. The supervisor records its location and resets the miss counter.
pub async fn send_heartbeat(
    supervisor: SocketAddr,
    agent: &AgentId,
    node: &NodeId,
    checkpoint: &Checkpoint,
) -> Result<(), TamError> {
    let hb = Heartbeat {
        agent: agent.clone(),
        node: node.0.clone(),
        checkpoint: checkpoint.clone(),
    };
    let bytes = serde_json::to_vec(&hb).map_err(|e| TamError::Invalid(e.to_string()))?;
    let mut s = TcpStream::connect(supervisor).await.map_err(io_err)?;
    s.write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .map_err(io_err)?;
    s.write_all(&bytes).await.map_err(io_err)?;
    s.flush().await.map_err(io_err)?;
    let mut ack = [0u8; 1];
    s.read_exact(&mut ack).await.map_err(io_err)?;
    Ok(())
}

/// Run a supervisor server: accept heartbeats over TCP and update the shared
/// [`Supervisor`]'s registry (location + last-good checkpoint + liveness).
/// Failure detection (`tick`) and `self_heal` remain the caller's policy
/// (TAM §4.2). Returns the bound address.
pub async fn serve_supervisor(
    supervisor: Arc<Mutex<Supervisor>>,
    addr: SocketAddr,
) -> Result<SocketAddr, TamError> {
    let listener = TcpListener::bind(addr).await.map_err(io_err)?;
    let bound = listener.local_addr().map_err(io_err)?;
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let mut len = [0u8; 4];
            if stream.read_exact(&mut len).await.is_err() {
                continue;
            }
            let n = u32::from_le_bytes(len) as usize;
            let mut buf = vec![0u8; n];
            if stream.read_exact(&mut buf).await.is_err() {
                continue;
            }
            if let Ok(hb) = serde_json::from_slice::<Heartbeat>(&buf) {
                let mut s = supervisor.lock().unwrap();
                s.observe(&hb.agent, NodeId::new(hb.node), hb.checkpoint);
                s.heartbeat(&hb.agent);
            }
            let _ = stream.write_all(&[1u8]).await;
        }
    });
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use thaliox_core::{CapabilityToken, Dtype, IntentGroup, MessageKind, Scope};

    use super::*;

    fn fp(model: &str) -> ModelFingerprint {
        ModelFingerprint {
            model_id: model.into(),
            revision: "1".into(),
            dim: 4,
        }
    }

    fn comm_cap(subject: &str, pattern: &str) -> CapabilityToken {
        CapabilityToken {
            subject: AgentId::new(subject),
            permissions: vec![Permission::Communicate],
            scope: vec![Scope {
                resource: ResourceKind::Agent,
                pattern: pattern.into(),
            }],
            issued_at: 0,
            expires_at: 0,
            jti: [0; 16],
            delegable: false,
            signature: [0; 32],
        }
    }

    fn msg(from: &str, to: Recipient, model: &str, cap: Option<CapabilityToken>) -> VectorMessage {
        VectorMessage {
            from: AgentId::new(from),
            to,
            fingerprint: fp(model),
            kind: MessageKind::Data,
            payload: VectorPayload::Dense {
                dtype: Dtype::Fp32,
                dim: 4,
                data: vec![0; 16],
            },
            intent: None,
            seq: 0,
            capability: cap,
        }
    }

    #[tokio::test]
    async fn unicast_delivers_with_capability() {
        let fabric = LocalFabric::new();
        let a = fabric.endpoint(AgentId::new("a"), fp("m1"));
        let b = fabric.endpoint(AgentId::new("b"), fp("m1"));
        let m = msg(
            "a",
            Recipient::Unicast(AgentId::new("b")),
            "m1",
            Some(comm_cap("a", "b")),
        );
        a.send(m).await.unwrap();
        assert_eq!(b.recv().await.unwrap().from, AgentId::new("a"));
    }

    #[tokio::test]
    async fn send_without_capability_is_denied() {
        let fabric = LocalFabric::new();
        let a = fabric.endpoint(AgentId::new("a"), fp("m1"));
        let _b = fabric.endpoint(AgentId::new("b"), fp("m1"));
        let r = a
            .send(msg("a", Recipient::Unicast(AgentId::new("b")), "m1", None))
            .await;
        assert!(matches!(r, Err(TamError::CapabilityDenied(_))));
    }

    #[tokio::test]
    async fn wrong_scope_capability_is_denied() {
        let fabric = LocalFabric::new();
        let a = fabric.endpoint(AgentId::new("a"), fp("m1"));
        // Cap authorizes talking to "c", but the message targets "b".
        let r = a
            .send(msg(
                "a",
                Recipient::Unicast(AgentId::new("b")),
                "m1",
                Some(comm_cap("a", "c")),
            ))
            .await;
        assert!(matches!(r, Err(TamError::CapabilityDenied(_))));
    }

    #[tokio::test]
    async fn multicast_delivers_to_group() {
        let fabric = LocalFabric::new();
        let a = fabric.endpoint(AgentId::new("a"), fp("m1"));
        let b = fabric.endpoint(AgentId::new("b"), fp("m1"));
        let c = fabric.endpoint(AgentId::new("c"), fp("m1"));
        fabric.join(&AgentId::new("b"), "team");
        fabric.join(&AgentId::new("c"), "team");
        a.send(msg(
            "a",
            Recipient::Multicast(IntentGroup("team".into())),
            "m1",
            Some(comm_cap("a", "team")),
        ))
        .await
        .unwrap();
        assert!(b.recv().await.is_ok());
        assert!(c.recv().await.is_ok());
    }

    #[test]
    fn inv3_fidelity_classification() {
        let receiver = fp("m1");
        // same fingerprint, aligned → lossless
        let same = msg("a", Recipient::Unicast(AgentId::new("b")), "m1", None);
        assert_eq!(fidelity(&same, &receiver), Fidelity::Lossless);
        // different fingerprint, aligned → needs translation (no implicit inject)
        let diff = msg("a", Recipient::Unicast(AgentId::new("b")), "m2", None);
        assert_eq!(fidelity(&diff, &receiver), Fidelity::NeedsTranslation);
        // raw payload → unaligned escape hatch
        let mut raw = diff.clone();
        raw.payload = VectorPayload::Raw {
            content_type: "text/plain".into(),
            bytes: b"hi".to_vec(),
        };
        assert_eq!(fidelity(&raw, &receiver), Fidelity::Unaligned);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn net_node_delivers_over_tcp() {
        let node_a = NetNode::new();
        let node_b = NetNode::new();
        let a = node_a.endpoint(AgentId::new("a"), fp("m1"));
        let b = node_b.endpoint(AgentId::new("b"), fp("m1"));

        // B listens; A learns the route to B over TCP.
        let addr_b = node_b.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        node_a.route(AgentId::new("b"), addr_b);

        a.send(msg(
            "a",
            Recipient::Unicast(AgentId::new("b")),
            "m1",
            Some(comm_cap("a", "b")),
        ))
        .await
        .unwrap();

        // Delivery is async — poll the inbox briefly.
        let mut got = None;
        for _ in 0..100 {
            if let Ok(m) = b.recv().await {
                got = Some(m);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(got.unwrap().from, AgentId::new("a"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn net_send_is_capability_gated() {
        let node_a = NetNode::new();
        let a = node_a.endpoint(AgentId::new("a"), fp("m1"));
        node_a.route(AgentId::new("b"), "127.0.0.1:1".parse().unwrap());
        // No capability → denied before any network hop.
        let r = a
            .send(msg("a", Recipient::Unicast(AgentId::new("b")), "m1", None))
            .await;
        assert!(matches!(r, Err(TamError::CapabilityDenied(_))));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn migrate_a_package_over_the_network() {
        use thaliox_cognition::MockProvider;
        use thaliox_core::AttentionBudget;
        use thaliox_memory::InMemorySpace;
        use thaliox_runtime::{Action, Agent, Manifest};

        // Source agent that did work → budget 95.
        let mut a = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(100, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        );
        a.start().unwrap();
        a.act(Action::Think {
            prompt: "w".into(),
            cost: 5,
        })
        .await
        .unwrap();
        let pkg = Package::pack(&a, Manifest::new(AgentId::new("a1")));

        // Destination node + its migration listener.
        let dest = Arc::new(Mutex::new(Node::new("B")));
        let env = || DeployEnv {
            memory: Arc::new(InMemorySpace::new()),
            mind: Arc::new(MockProvider::new("ok", 5)),
            tools: vec![],
            verifier: None,
        };
        let addr = serve_migrations(dest.clone(), env, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        // Migrate over TCP; the ack means it was deployed on the destination.
        send_migration(addr, &pkg).await.unwrap();

        let id = AgentId::new("a1");
        for _ in 0..100 {
            if dest.lock().unwrap().hosts(&id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let node = dest.lock().unwrap();
        let moved = node.agent(&id).expect("agent migrated to dest");
        assert_eq!(moved.remaining_budget(), 95); // migrated state, not reset
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn distributed_heartbeat_detects_failure() {
        use thaliox_cognition::MockProvider;
        use thaliox_core::AttentionBudget;
        use thaliox_memory::InMemorySpace;
        use thaliox_runtime::{Agent, Health};

        // A checkpoint to report.
        let mut agent = Agent::new(
            AgentId::new("a1"),
            AttentionBudget::new(100, 1000),
            Arc::new(InMemorySpace::new()),
            Arc::new(MockProvider::new("ok", 5)),
        );
        agent.start().unwrap();
        let ckpt = agent.checkpoint();

        // Supervisor server (3 missed beats ⇒ suspected).
        let sup = Arc::new(Mutex::new(Supervisor::new(3)));
        let addr = serve_supervisor(sup.clone(), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        // Node A heartbeats over TCP; the ack means the supervisor recorded it.
        let id = AgentId::new("a1");
        send_heartbeat(addr, &id, &NodeId::new("A"), &ckpt)
            .await
            .unwrap();
        assert_eq!(sup.lock().unwrap().node_of(&id), Some(&NodeId::new("A")));
        assert_eq!(sup.lock().unwrap().health(&id), Some(Health::Healthy));

        // Node A goes silent — three ticks later it is suspected down.
        {
            let mut s = sup.lock().unwrap();
            s.tick();
            s.tick();
            assert!(s.tick().contains(&id));
        }
        assert_eq!(sup.lock().unwrap().health(&id), Some(Health::Suspected));
    }
}
