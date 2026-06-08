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
    AgentId, CapabilityToken, IntentGroup, MessageKind, ModelFingerprint, Permission, Recipient,
    ResourceKind, TamError, VectorMessage, VectorPayload,
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
    /// Join a multicast intent group (used by the Swarm paradigm).
    pub fn join(&self, group: &str) {
        self.fabric.join(&self.id, group);
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

// ---------- M4c: team execution — the Pipeline paradigm (RFC-0006 §4) ----------

/// One stage of a [`Pipeline`]: the per-message behavior of the agent at this
/// position. `process` stands in for the agent's model forward-pass — it maps an
/// inbound [`VectorMessage`] to the [`VectorPayload`] this agent forwards on.
/// (A real model call when cognition lands; today a pure transform so the team
/// orchestration is CI-testable without the full mind.)
#[async_trait]
pub trait Stage: Send + Sync {
    /// The agent backing this stage (its team member id).
    fn agent(&self) -> &AgentId;
    /// Transform the inbound message into this agent's output payload.
    async fn process(&self, msg: &VectorMessage) -> Result<VectorPayload, TamError>;
}

/// Fuses several payloads into one — a Hierarchy lead's aggregator or a Swarm's
/// consensus function.
type Fuse = Box<dyn Fn(&[VectorPayload]) -> VectorPayload + Send + Sync>;

/// A [`Stage`] adapter over a plain synchronous transform — the common case and
/// what tests use. Async stages (e.g. ones that call a model) implement [`Stage`]
/// directly.
pub struct MapStage<F> {
    agent: AgentId,
    f: F,
}

impl<F> MapStage<F>
where
    F: Fn(&VectorMessage) -> VectorPayload + Send + Sync,
{
    pub fn new(agent: AgentId, f: F) -> Self {
        Self { agent, f }
    }
}

#[async_trait]
impl<F> Stage for MapStage<F>
where
    F: Fn(&VectorMessage) -> VectorPayload + Send + Sync,
{
    fn agent(&self) -> &AgentId {
        &self.agent
    }
    async fn process(&self, msg: &VectorMessage) -> Result<VectorPayload, TamError> {
        Ok((self.f)(msg))
    }
}

/// **INV-3 guard**, shared by every paradigm: a stage/agent must not have a
/// cross-`ModelFingerprint` aligned payload injected implicitly — it has to be
/// marked `Translate` first. Returns an error naming the agent otherwise.
fn inv3_guard(
    msg: &VectorMessage,
    receiver: &ModelFingerprint,
    who: &AgentId,
) -> Result<(), TamError> {
    if matches!(fidelity(msg, receiver), Fidelity::NeedsTranslation)
        && msg.kind != MessageKind::Translate
    {
        return Err(TamError::Invalid(format!(
            "INV-3: {who} would inject a cross-fingerprint payload without translation"
        )));
    }
    Ok(())
}

struct PipeStage {
    stage: Box<dyn Stage>,
    endpoint: Endpoint,
    /// Capability this stage uses to forward to the *next* stage (INV-2). The
    /// final stage has none.
    cap_to_next: Option<CapabilityToken>,
}

/// A team running the **Pipeline** paradigm: a chain of agents over the fabric,
/// each transforming a [`VectorMessage`] and forwarding it to the next
/// (RFC-0006 §4, the simplest paradigm to land first).
///
/// Execution threads the message through the members in order. **Every hop is a
/// real capability-gated fabric `send`** — so INV-2 is enforced *between* team
/// members, not just at the team boundary. **INV-3 is checked on entry to each
/// stage**: a cross-`ModelFingerprint` aligned payload must be explicitly marked
/// `Translate`, never implicitly injected.
pub struct Pipeline {
    name: String,
    stages: Vec<PipeStage>,
}

impl Pipeline {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            stages: Vec::new(),
        }
    }

    /// Append a stage: the agent's behavior, its fabric [`Endpoint`], and the
    /// capability it forwards to the next stage with (`None` for the last stage).
    pub fn stage(
        mut self,
        stage: Box<dyn Stage>,
        endpoint: Endpoint,
        cap_to_next: Option<CapabilityToken>,
    ) -> Self {
        self.stages.push(PipeStage {
            stage,
            endpoint,
            cap_to_next,
        });
        self
    }

    /// The declarative [`Team`] this pipeline realizes (members in stage order).
    pub fn team(&self) -> Team {
        Team {
            name: self.name.clone(),
            members: self
                .stages
                .iter()
                .map(|s| s.stage.agent().clone())
                .collect(),
            paradigm: Paradigm::Pipeline,
        }
    }

    /// Run `input` through the chain, returning the final stage's output message.
    pub async fn run(&self, input: VectorMessage) -> Result<VectorMessage, TamError> {
        if self.stages.is_empty() {
            return Err(TamError::Invalid("empty pipeline".into()));
        }
        let mut current = input;
        for i in 0..self.stages.len() {
            let s = &self.stages[i];

            // INV-3: forbid implicit cross-fingerprint injection at this stage.
            inv3_guard(&current, s.endpoint.fingerprint(), s.endpoint.id())?;

            // The agent computes its output payload.
            let payload = s.stage.process(&current).await?;
            let mut out = current.clone();
            out.from = s.endpoint.id().clone();
            out.fingerprint = s.endpoint.fingerprint().clone();
            out.payload = payload;
            out.seq += 1;

            // Last stage → the pipeline's result.
            if i + 1 == self.stages.len() {
                return Ok(out);
            }

            // Forward to the next stage over the fabric (INV-2 gated by `send`).
            let next_id = self.stages[i + 1].endpoint.id().clone();
            out.to = Recipient::Unicast(next_id);
            out.capability = s.cap_to_next.clone();
            self.stages[i].endpoint.send(out).await?;
            current = self.stages[i + 1].endpoint.recv().await?;
        }
        unreachable!("returned at the last stage")
    }
}

// ---------- M4c: the Hierarchy paradigm (RFC-0006 §4) ----------

struct HierarchyChild {
    stage: Box<dyn Stage>,
    endpoint: Endpoint,
    /// Capability the child reports back up to the lead with (INV-2).
    cap_to_lead: Option<CapabilityToken>,
}

/// A team running the **Hierarchy** paradigm: a lead agent delegates the task to
/// each child and aggregates their reports (RFC-0006 §4). Both legs — lead→child
/// delegation and child→lead report — are capability-gated fabric hops (INV-2),
/// and each child applies the INV-3 entry guard on what it is handed.
pub struct Hierarchy {
    name: String,
    lead: Endpoint,
    children: Vec<HierarchyChild>,
    /// The lead's capability to delegate to its children (INV-2).
    cap_to_child: Option<CapabilityToken>,
    /// How the lead fuses the children's reports into its answer.
    aggregate: Fuse,
}

impl Hierarchy {
    pub fn new(
        name: impl Into<String>,
        lead: Endpoint,
        cap_to_child: Option<CapabilityToken>,
        aggregate: impl Fn(&[VectorPayload]) -> VectorPayload + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            lead,
            children: Vec::new(),
            cap_to_child,
            aggregate: Box::new(aggregate),
        }
    }

    /// Add a child agent that processes a delegated task and reports back.
    pub fn child(
        mut self,
        stage: Box<dyn Stage>,
        endpoint: Endpoint,
        cap_to_lead: Option<CapabilityToken>,
    ) -> Self {
        self.children.push(HierarchyChild {
            stage,
            endpoint,
            cap_to_lead,
        });
        self
    }

    /// The declarative [`Team`]: the lead first, then the children.
    pub fn team(&self) -> Team {
        let mut members = vec![self.lead.id().clone()];
        members.extend(self.children.iter().map(|c| c.endpoint.id().clone()));
        Team {
            name: self.name.clone(),
            members,
            paradigm: Paradigm::Hierarchy,
        }
    }

    /// Delegate `task` to every child, then return the lead's aggregated answer.
    pub async fn run(&self, task: VectorMessage) -> Result<VectorMessage, TamError> {
        let mut reports = Vec::with_capacity(self.children.len());
        for child in &self.children {
            // Lead delegates down (INV-2).
            let mut down = task.clone();
            down.from = self.lead.id().clone();
            down.to = Recipient::Unicast(child.endpoint.id().clone());
            down.capability = self.cap_to_child.clone();
            self.lead.send(down).await?;

            let got = child.endpoint.recv().await?;
            inv3_guard(&got, child.endpoint.fingerprint(), child.endpoint.id())?;
            let payload = child.stage.process(&got).await?;

            // Child reports up (INV-2).
            let mut up = got;
            up.from = child.endpoint.id().clone();
            up.fingerprint = child.endpoint.fingerprint().clone();
            up.payload = payload;
            up.to = Recipient::Unicast(self.lead.id().clone());
            up.capability = child.cap_to_lead.clone();
            child.endpoint.send(up).await?;
            reports.push(self.lead.recv().await?.payload);
        }
        let mut answer = task;
        answer.from = self.lead.id().clone();
        answer.fingerprint = self.lead.fingerprint().clone();
        answer.payload = (self.aggregate)(&reports);
        Ok(answer)
    }
}

// ---------- M4c: the Market paradigm (RFC-0006 §4) ----------

/// A market participant: it bids a cost for a task (lower wins) and, if it wins
/// the auction, executes it. Bidding and execution are separated so the
/// auctioneer can compare costs before committing work.
#[async_trait]
pub trait Bidder: Send + Sync {
    fn agent(&self) -> &AgentId;
    /// Estimated cost to handle `task` — the lowest bid wins.
    async fn bid(&self, task: &VectorMessage) -> Result<u64, TamError>;
    /// Produce the result (only the winner is asked).
    async fn execute(&self, task: &VectorMessage) -> Result<VectorPayload, TamError>;
}

const BID_CONTENT_TYPE: &str = "application/x-thaliox-bid";

fn encode_bid(cost: u64) -> VectorPayload {
    VectorPayload::Raw {
        content_type: BID_CONTENT_TYPE.into(),
        bytes: cost.to_le_bytes().to_vec(),
    }
}

fn decode_bid(msg: &VectorMessage) -> Result<u64, TamError> {
    match &msg.payload {
        VectorPayload::Raw {
            content_type,
            bytes,
        } if content_type == BID_CONTENT_TYPE && bytes.len() == 8 => {
            Ok(u64::from_le_bytes(bytes.as_slice().try_into().unwrap()))
        }
        _ => Err(TamError::Invalid("malformed bid reply".into())),
    }
}

struct MarketBidder {
    bidder: Box<dyn Bidder>,
    endpoint: Endpoint,
    /// Capability the bidder replies to the auctioneer with (INV-2).
    cap_to_auctioneer: Option<CapabilityToken>,
}

/// A team running the **Market** paradigm: an auctioneer announces a task, each
/// bidder replies with a cost, and the lowest bidder is assigned the work
/// (RFC-0006 §4). The task announcement and each bid reply are capability-gated
/// fabric hops (INV-2).
pub struct Market {
    name: String,
    auctioneer: Endpoint,
    bidders: Vec<MarketBidder>,
    /// The auctioneer's capability to announce to bidders (INV-2).
    cap_to_bidder: Option<CapabilityToken>,
}

impl Market {
    pub fn new(
        name: impl Into<String>,
        auctioneer: Endpoint,
        cap_to_bidder: Option<CapabilityToken>,
    ) -> Self {
        Self {
            name: name.into(),
            auctioneer,
            bidders: Vec::new(),
            cap_to_bidder,
        }
    }

    pub fn bidder(
        mut self,
        bidder: Box<dyn Bidder>,
        endpoint: Endpoint,
        cap_to_auctioneer: Option<CapabilityToken>,
    ) -> Self {
        self.bidders.push(MarketBidder {
            bidder,
            endpoint,
            cap_to_auctioneer,
        });
        self
    }

    pub fn team(&self) -> Team {
        let mut members = vec![self.auctioneer.id().clone()];
        members.extend(self.bidders.iter().map(|b| b.endpoint.id().clone()));
        Team {
            name: self.name.clone(),
            members,
            paradigm: Paradigm::Market,
        }
    }

    /// Run the auction: returns the winning agent and its result message.
    pub async fn run(&self, task: VectorMessage) -> Result<(AgentId, VectorMessage), TamError> {
        let mut best: Option<(usize, u64)> = None;
        for (i, b) in self.bidders.iter().enumerate() {
            // Announce the task to the bidder (INV-2).
            let mut ann = task.clone();
            ann.from = self.auctioneer.id().clone();
            ann.to = Recipient::Unicast(b.endpoint.id().clone());
            ann.capability = self.cap_to_bidder.clone();
            self.auctioneer.send(ann).await?;

            let req = b.endpoint.recv().await?;
            let cost = b.bidder.bid(&req).await?;

            // Bidder replies its bid over the fabric (INV-2).
            let mut reply = req;
            reply.from = b.endpoint.id().clone();
            reply.to = Recipient::Unicast(self.auctioneer.id().clone());
            reply.payload = encode_bid(cost);
            reply.capability = b.cap_to_auctioneer.clone();
            b.endpoint.send(reply).await?;

            let got = self.auctioneer.recv().await?;
            let bid = decode_bid(&got)?;
            if best.is_none_or(|(_, c)| bid < c) {
                best = Some((i, bid));
            }
        }
        let (wi, _) = best.ok_or_else(|| TamError::Invalid("no bidders".into()))?;
        let winner = &self.bidders[wi];
        let payload = winner.bidder.execute(&task).await?;
        let mut result = task;
        result.from = winner.endpoint.id().clone();
        result.fingerprint = winner.endpoint.fingerprint().clone();
        result.payload = payload;
        Ok((winner.bidder.agent().clone(), result))
    }
}

// ---------- M4c: the Swarm paradigm (RFC-0006 §4) ----------

struct SwarmPeer {
    stage: Box<dyn Stage>,
    endpoint: Endpoint,
    /// Capability the peer broadcasts to the group with (INV-2).
    cap_to_group: Option<CapabilityToken>,
}

/// A team running the **Swarm** paradigm: peers broadcast a proposal to a shared
/// intent group and converge on an emergent consensus (RFC-0006 §4). Every
/// broadcast is a capability-gated multicast (INV-2); proposals must share a
/// `ModelFingerprint` to be fused (INV-3) — you cannot average across vector
/// spaces without translation.
pub struct Swarm {
    name: String,
    group: String,
    peers: Vec<SwarmPeer>,
    /// How the swarm fuses all proposals into a consensus payload.
    consensus: Fuse,
}

impl Swarm {
    pub fn new(
        name: impl Into<String>,
        group: impl Into<String>,
        consensus: impl Fn(&[VectorPayload]) -> VectorPayload + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            group: group.into(),
            peers: Vec::new(),
            consensus: Box::new(consensus),
        }
    }

    /// Add a peer; it joins the group immediately so it receives broadcasts.
    pub fn peer(
        mut self,
        stage: Box<dyn Stage>,
        endpoint: Endpoint,
        cap_to_group: Option<CapabilityToken>,
    ) -> Self {
        endpoint.join(&self.group);
        self.peers.push(SwarmPeer {
            stage,
            endpoint,
            cap_to_group,
        });
        self
    }

    pub fn team(&self) -> Team {
        Team {
            name: self.name.clone(),
            members: self.peers.iter().map(|p| p.endpoint.id().clone()).collect(),
            paradigm: Paradigm::Swarm,
        }
    }

    /// Each peer proposes (from `input`) and broadcasts to the group; the swarm
    /// then fuses all proposals into a consensus message.
    pub async fn run(&self, input: VectorMessage) -> Result<VectorMessage, TamError> {
        if self.peers.is_empty() {
            return Err(TamError::Invalid("empty swarm".into()));
        }
        // Each peer computes and broadcasts its proposal (INV-2 multicast).
        for peer in &self.peers {
            let proposal = peer.stage.process(&input).await?;
            let mut out = input.clone();
            out.from = peer.endpoint.id().clone();
            out.fingerprint = peer.endpoint.fingerprint().clone();
            out.payload = proposal;
            out.to = Recipient::Multicast(IntentGroup(self.group.clone()));
            out.capability = peer.cap_to_group.clone();
            peer.endpoint.send(out).await?;
        }
        // Every peer is a group member, so peer[0]'s inbox now holds all
        // proposals. Drain them, enforcing INV-3 (one shared vector space).
        let reference = self.peers[0].endpoint.fingerprint().clone();
        let mut proposals = Vec::with_capacity(self.peers.len());
        while let Ok(m) = self.peers[0].endpoint.recv().await {
            inv3_guard(&m, &reference, self.peers[0].endpoint.id())?;
            proposals.push(m.payload);
        }
        let mut answer = input;
        answer.from = self.peers[0].endpoint.id().clone();
        answer.fingerprint = reference;
        answer.payload = (self.consensus)(&proposals);
        Ok(answer)
    }
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

    // ---------- M4c: Pipeline team execution ----------

    /// A stage whose agent adds 1 to every byte of a `Dense` payload (a stand-in
    /// transform). Lets a chain produce a verifiable end-to-end result.
    fn add_one(id: &str) -> Box<dyn Stage> {
        Box::new(MapStage::new(
            AgentId::new(id),
            |m: &VectorMessage| match &m.payload {
                VectorPayload::Dense { dtype, dim, data } => VectorPayload::Dense {
                    dtype: *dtype,
                    dim: *dim,
                    data: data.iter().map(|b| b.wrapping_add(1)).collect(),
                },
                other => other.clone(),
            },
        ))
    }

    #[tokio::test]
    async fn pipeline_threads_and_transforms() {
        let fab = LocalFabric::new();
        let e1 = fab.endpoint(AgentId::new("s1"), fp("m1"));
        let e2 = fab.endpoint(AgentId::new("s2"), fp("m1"));
        let e3 = fab.endpoint(AgentId::new("s3"), fp("m1"));
        let pipe = Pipeline::new("etl")
            .stage(add_one("s1"), e1, Some(comm_cap("s1", "s2")))
            .stage(add_one("s2"), e2, Some(comm_cap("s2", "s3")))
            .stage(add_one("s3"), e3, None);

        // Declarative team shape (members in stage order).
        let team = pipe.team();
        assert_eq!(team.paradigm, Paradigm::Pipeline);
        assert_eq!(
            team.members,
            vec![AgentId::new("s1"), AgentId::new("s2"), AgentId::new("s3")]
        );

        let input = msg("client", Recipient::Unicast(AgentId::new("s1")), "m1", None);
        let out = pipe.run(input).await.unwrap();

        // The output came from the last stage and was transformed three times.
        assert_eq!(out.from, AgentId::new("s3"));
        match out.payload {
            VectorPayload::Dense { data, .. } => assert!(data.iter().all(|&b| b == 3)),
            _ => panic!("expected dense payload"),
        }
    }

    #[tokio::test]
    async fn pipeline_hop_is_capability_gated() {
        let fab = LocalFabric::new();
        let e1 = fab.endpoint(AgentId::new("s1"), fp("m1"));
        let e2 = fab.endpoint(AgentId::new("s2"), fp("m1"));
        // s1's forwarding capability authorizes talking to "x", not "s2".
        let pipe = Pipeline::new("p")
            .stage(add_one("s1"), e1, Some(comm_cap("s1", "x")))
            .stage(add_one("s2"), e2, None);

        let r = pipe
            .run(msg("c", Recipient::Unicast(AgentId::new("s1")), "m1", None))
            .await;
        assert!(matches!(r, Err(TamError::CapabilityDenied(_))));
    }

    #[tokio::test]
    async fn pipeline_enforces_inv3_translation() {
        // A stage in a different vector space (m2) than the inbound message (m1).
        let build = || {
            let fab = LocalFabric::new();
            let e1 = fab.endpoint(AgentId::new("s1"), fp("m1"));
            let e2 = fab.endpoint(AgentId::new("s2"), fp("m2"));
            Pipeline::new("x")
                .stage(add_one("s1"), e1, Some(comm_cap("s1", "s2")))
                .stage(add_one("s2"), e2, None)
        };

        // Data crossing into m2 without a Translate is rejected (INV-3).
        let denied = build()
            .run(msg("c", Recipient::Unicast(AgentId::new("s1")), "m1", None))
            .await;
        assert!(matches!(denied, Err(TamError::Invalid(m)) if m.contains("INV-3")));

        // Marking the message Translate makes the cross-fingerprint hop explicit.
        let mut translated = msg("c", Recipient::Unicast(AgentId::new("s1")), "m1", None);
        translated.kind = MessageKind::Translate;
        assert!(build().run(translated).await.is_ok());
    }

    // ---------- M4c: Hierarchy ----------

    fn dense(val: u8) -> VectorPayload {
        VectorPayload::Dense {
            dtype: Dtype::Fp32,
            dim: 4,
            data: vec![val; 16],
        }
    }

    /// A stage whose agent ignores its input and proposes a constant vector.
    fn const_stage(id: &str, val: u8) -> Box<dyn Stage> {
        Box::new(MapStage::new(AgentId::new(id), move |_: &VectorMessage| {
            dense(val)
        }))
    }

    /// Element-wise sum of `Dense` payloads (a stand-in aggregator).
    fn sum_dense(parts: &[VectorPayload]) -> VectorPayload {
        let mut acc = vec![0u8; 16];
        for p in parts {
            if let VectorPayload::Dense { data, .. } = p {
                for (a, b) in acc.iter_mut().zip(data) {
                    *a = a.wrapping_add(*b);
                }
            }
        }
        VectorPayload::Dense {
            dtype: Dtype::Fp32,
            dim: 4,
            data: acc,
        }
    }

    #[tokio::test]
    async fn hierarchy_delegates_and_aggregates() {
        let fab = LocalFabric::new();
        let lead = fab.endpoint(AgentId::new("lead"), fp("m1"));
        let c1 = fab.endpoint(AgentId::new("c1"), fp("m1"));
        let c2 = fab.endpoint(AgentId::new("c2"), fp("m1"));
        // Input bytes are 0; each child adds 1 → reports of 1; lead sums → 2.
        let team = Hierarchy::new("h", lead, Some(comm_cap("lead", "*")), sum_dense)
            .child(add_one("c1"), c1, Some(comm_cap("c1", "lead")))
            .child(add_one("c2"), c2, Some(comm_cap("c2", "lead")));

        let t = team.team();
        assert_eq!(t.paradigm, Paradigm::Hierarchy);
        assert_eq!(t.members[0], AgentId::new("lead"));

        let out = team
            .run(msg(
                "client",
                Recipient::Unicast(AgentId::new("lead")),
                "m1",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(out.from, AgentId::new("lead"));
        match out.payload {
            VectorPayload::Dense { data, .. } => assert!(data.iter().all(|&b| b == 2)),
            _ => panic!("expected dense"),
        }
    }

    #[tokio::test]
    async fn hierarchy_child_report_is_gated() {
        let fab = LocalFabric::new();
        let lead = fab.endpoint(AgentId::new("lead"), fp("m1"));
        let c1 = fab.endpoint(AgentId::new("c1"), fp("m1"));
        // Child's report capability targets "x", not "lead" → denied on report.
        let team = Hierarchy::new("h", lead, Some(comm_cap("lead", "*")), sum_dense).child(
            add_one("c1"),
            c1,
            Some(comm_cap("c1", "x")),
        );
        let r = team
            .run(msg(
                "client",
                Recipient::Unicast(AgentId::new("lead")),
                "m1",
                None,
            ))
            .await;
        assert!(matches!(r, Err(TamError::CapabilityDenied(_))));
    }

    // ---------- M4c: Market ----------

    struct FlatBidder {
        id: AgentId,
        cost: u64,
    }

    #[async_trait]
    impl Bidder for FlatBidder {
        fn agent(&self) -> &AgentId {
            &self.id
        }
        async fn bid(&self, _task: &VectorMessage) -> Result<u64, TamError> {
            Ok(self.cost)
        }
        async fn execute(&self, _task: &VectorMessage) -> Result<VectorPayload, TamError> {
            Ok(VectorPayload::Raw {
                content_type: "text/plain".into(),
                bytes: self.id.0.clone().into_bytes(),
            })
        }
    }

    fn flat(id: &str, cost: u64) -> Box<dyn Bidder> {
        Box::new(FlatBidder {
            id: AgentId::new(id),
            cost,
        })
    }

    #[tokio::test]
    async fn market_lowest_bid_wins() {
        let fab = LocalFabric::new();
        let auct = fab.endpoint(AgentId::new("auct"), fp("m1"));
        let b1 = fab.endpoint(AgentId::new("b1"), fp("m1"));
        let b2 = fab.endpoint(AgentId::new("b2"), fp("m1"));
        let market = Market::new("m", auct, Some(comm_cap("auct", "*")))
            .bidder(flat("b1", 10), b1, Some(comm_cap("b1", "auct")))
            .bidder(flat("b2", 3), b2, Some(comm_cap("b2", "auct")));

        assert_eq!(market.team().paradigm, Paradigm::Market);

        let (winner, result) = market
            .run(msg(
                "client",
                Recipient::Unicast(AgentId::new("auct")),
                "m1",
                None,
            ))
            .await
            .unwrap();
        // b2 bid 3 < b1 bid 10 → b2 wins and executes.
        assert_eq!(winner, AgentId::new("b2"));
        assert_eq!(result.from, AgentId::new("b2"));
        match result.payload {
            VectorPayload::Raw { bytes, .. } => assert_eq!(bytes, b"b2"),
            _ => panic!("expected raw result"),
        }
    }

    #[tokio::test]
    async fn market_announce_is_gated() {
        let fab = LocalFabric::new();
        let auct = fab.endpoint(AgentId::new("auct"), fp("m1"));
        let b1 = fab.endpoint(AgentId::new("b1"), fp("m1"));
        // Auctioneer's announce capability targets "other", not bidder "b1".
        let market = Market::new("m", auct, Some(comm_cap("auct", "other"))).bidder(
            flat("b1", 1),
            b1,
            Some(comm_cap("b1", "auct")),
        );
        let r = market
            .run(msg(
                "c",
                Recipient::Unicast(AgentId::new("auct")),
                "m1",
                None,
            ))
            .await;
        assert!(matches!(r, Err(TamError::CapabilityDenied(_))));
    }

    // ---------- M4c: Swarm ----------

    /// Element-wise integer mean of `Dense` payloads (the emergent consensus).
    fn mean_dense(props: &[VectorPayload]) -> VectorPayload {
        let n = props.len().max(1) as u32;
        let mut sum = [0u32; 16];
        for p in props {
            if let VectorPayload::Dense { data, .. } = p {
                for (a, b) in sum.iter_mut().zip(data) {
                    *a += *b as u32;
                }
            }
        }
        VectorPayload::Dense {
            dtype: Dtype::Fp32,
            dim: 4,
            data: sum.iter().map(|s| (s / n) as u8).collect(),
        }
    }

    #[tokio::test]
    async fn swarm_reaches_consensus() {
        let fab = LocalFabric::new();
        let p1 = fab.endpoint(AgentId::new("p1"), fp("m1"));
        let p2 = fab.endpoint(AgentId::new("p2"), fp("m1"));
        let p3 = fab.endpoint(AgentId::new("p3"), fp("m1"));
        // Proposals 2, 4, 6 → mean 4.
        let swarm = Swarm::new("s", "flock", mean_dense)
            .peer(const_stage("p1", 2), p1, Some(comm_cap("p1", "flock")))
            .peer(const_stage("p2", 4), p2, Some(comm_cap("p2", "flock")))
            .peer(const_stage("p3", 6), p3, Some(comm_cap("p3", "flock")));

        assert_eq!(swarm.team().paradigm, Paradigm::Swarm);

        let out = swarm
            .run(msg(
                "seed",
                Recipient::Unicast(AgentId::new("p1")),
                "m1",
                None,
            ))
            .await
            .unwrap();
        match out.payload {
            VectorPayload::Dense { data, .. } => assert!(data.iter().all(|&b| b == 4)),
            _ => panic!("expected dense consensus"),
        }
    }

    #[tokio::test]
    async fn swarm_broadcast_is_gated() {
        let fab = LocalFabric::new();
        let p1 = fab.endpoint(AgentId::new("p1"), fp("m1"));
        let p2 = fab.endpoint(AgentId::new("p2"), fp("m1"));
        // p1's broadcast capability targets the wrong group → denied.
        let swarm = Swarm::new("s", "flock", mean_dense)
            .peer(const_stage("p1", 2), p1, Some(comm_cap("p1", "wrong")))
            .peer(const_stage("p2", 4), p2, Some(comm_cap("p2", "flock")));
        let r = swarm
            .run(msg(
                "seed",
                Recipient::Unicast(AgentId::new("p1")),
                "m1",
                None,
            ))
            .await;
        assert!(matches!(r, Err(TamError::CapabilityDenied(_))));
    }
}
