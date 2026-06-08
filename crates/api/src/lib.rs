//! # THALIOX api (L5) — the cluster front door
//!
//! An axum service that maps external requests onto the agent runtime: spawn an
//! agent, then drive it (`think` / `remember` / `recall` / `invoke`) and read
//! its `audit`. Every request bottoms out in [`Agent::act`](thaliox_runtime::Agent)
//! — so the TAM gates (INV-1 budget · INV-2 capability · INV-4 audit) apply to
//! HTTP callers too. (MASTER_PLAN §3 item 11, F6.)
//!
//! **M4d (RFC-0006 §5)** generalizes it from one agent's API into the *cluster's
//! front door*:
//! - **Multiple client surfaces, one model** — request/response JSON for web &
//!   tools, plus a Server-Sent-Events stream (`/agents/{id}/events`) for live
//!   I/O. Both go through the same authorization.
//! - **One authorization model (INV-2 at the door)** — in cluster mode the
//!   gateway admits a request only if it carries a `CapabilityToken`
//!   (`x-thaliox-capability` header) granting `Communicate` over the target
//!   agent, *before* dispatch. Open mode (the default) admits all, for local dev.
//! - **Cluster routing** — agents placed on a peer node are answered with a
//!   `307` to that node's gateway, so the fleet is reachable through one door
//!   regardless of where an agent lives (`GET /cluster` shows the topology).
//!
//! ```no_run
//! # use std::sync::Arc;
//! # async fn demo(memory: Arc<dyn thaliox_core::SemanticSpace>,
//! #               mind: Arc<dyn thaliox_cognition::LlmProvider>) {
//! let state = thaliox_api::GatewayState::new(memory, mind, vec![]);
//! thaliox_api::serve(state, "127.0.0.1:8088").await.unwrap();
//! # }
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::header::LOCATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use thaliox_cognition::LlmProvider;
use thaliox_core::{
    AgentId, AttentionBudget, CapabilityToken, Permission, ResourceKind, Scope, SemanticObject,
    SemanticSpace, TamError, Tool,
};
use thaliox_runtime::{Action, Agent, NodeId, Outcome};

/// Shared gateway state: a memory, a cognition backend, a tool set, and the live
/// agent table. Each spawned agent is wrapped in an async mutex (its `act` is
/// `&mut`); the table itself is a plain mutex held only briefly.
///
/// In **cluster mode** it also carries this node's id, a directory of agents
/// placed on peer nodes, the peers' base URLs, and whether the door requires a
/// capability (INV-2 admission).
pub struct GatewayState {
    node: NodeId,
    require_caps: bool,
    memory: Arc<dyn SemanticSpace>,
    mind: Arc<dyn LlmProvider>,
    tools: Vec<Arc<dyn Tool>>,
    agents: Mutex<HashMap<String, Arc<AsyncMutex<Agent>>>>,
    /// agent id → the peer node it lives on (only remote placements).
    directory: Mutex<HashMap<String, String>>,
    /// peer node id → its gateway base URL.
    peers: Mutex<HashMap<String, String>>,
}

/// Where an agent lives relative to this gateway.
enum Located {
    /// Hosted here — drive it directly.
    Local(Arc<AsyncMutex<Agent>>),
    /// On a peer node — the caller is redirected to this URL.
    Remote(String),
}

impl GatewayState {
    /// Build an **open** gateway (admits all callers) on node `"local"` — the
    /// local-dev default, unchanged from M1.
    pub fn new(
        memory: Arc<dyn SemanticSpace>,
        mind: Arc<dyn LlmProvider>,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Arc<Self> {
        Self::cluster(memory, mind, tools, "local", false)
    }

    /// Build a **cluster front door**: `node` names this gateway; when
    /// `require_caps` is set, every agent-scoped request must present a
    /// `CapabilityToken` granting `Communicate` over the target (INV-2).
    pub fn cluster(
        memory: Arc<dyn SemanticSpace>,
        mind: Arc<dyn LlmProvider>,
        tools: Vec<Arc<dyn Tool>>,
        node: impl Into<String>,
        require_caps: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            node: NodeId::new(node),
            require_caps,
            memory,
            mind,
            tools,
            agents: Mutex::new(HashMap::new()),
            directory: Mutex::new(HashMap::new()),
            peers: Mutex::new(HashMap::new()),
        })
    }

    /// Register a peer node's gateway base URL (e.g. `http://host-b:8088`).
    pub fn register_peer(&self, node: impl Into<String>, base_url: impl Into<String>) {
        self.peers
            .lock()
            .unwrap()
            .insert(node.into(), base_url.into());
    }

    /// Record that an agent lives on a peer node — requests for it are routed
    /// there (front-door redirect).
    pub fn place_remote(&self, agent: impl Into<String>, node: impl Into<String>) {
        self.directory
            .lock()
            .unwrap()
            .insert(agent.into(), node.into());
    }

    /// Resolve an agent to a local handle or a redirect URL (`suffix` is the
    /// operation sub-path, e.g. `"/think"`). Remote placement wins.
    fn locate(&self, id: &str, suffix: &str) -> Result<Located, ApiError> {
        if let Some(node) = self.directory.lock().unwrap().get(id).cloned() {
            let base = self
                .peers
                .lock()
                .unwrap()
                .get(&node)
                .cloned()
                .ok_or_else(|| {
                    ApiError::Tam(TamError::Invalid(format!("no route to node '{node}'")))
                })?;
            let url = format!("{}/agents/{}{}", base.trim_end_matches('/'), id, suffix);
            return Ok(Located::Remote(url));
        }
        self.agents
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .map(Located::Local)
            .ok_or_else(|| ApiError::NotFound(format!("agent '{id}'")))
    }

    /// INV-2 admission at the door: in cluster mode the request must carry an
    /// `x-thaliox-capability` token granting `perm` over `target`. Open mode
    /// admits all.
    fn admit(&self, headers: &HeaderMap, perm: Permission, target: &str) -> Result<(), ApiError> {
        if !self.require_caps {
            return Ok(());
        }
        let raw = headers
            .get("x-thaliox-capability")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                ApiError::Tam(TamError::CapabilityDenied(format!(
                    "missing capability for {target}"
                )))
            })?;
        let cap: CapabilityToken = serde_json::from_str(raw)
            .map_err(|e| ApiError::Tam(TamError::Invalid(format!("bad capability header: {e}"))))?;
        if cap.authorizes(perm, ResourceKind::Agent, target) {
            Ok(())
        } else {
            Err(ApiError::Tam(TamError::CapabilityDenied(format!(
                "{perm:?} on agent {target}"
            ))))
        }
    }
}

/// Maps a [`TamError`] / not-found / cluster-redirect onto an HTTP response.
pub enum ApiError {
    NotFound(String),
    Tam(TamError),
    /// The agent lives on a peer node — `307` the caller to its gateway.
    Located(String),
}

impl From<TamError> for ApiError {
    fn from(e: TamError) -> Self {
        ApiError::Tam(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Cluster front-door redirect: 307 preserves method + body.
        if let ApiError::Located(url) = self {
            return (StatusCode::TEMPORARY_REDIRECT, [(LOCATION, url)]).into_response();
        }
        let (status, msg) = match self {
            ApiError::Located(_) => unreachable!("handled above"),
            ApiError::NotFound(s) => (StatusCode::NOT_FOUND, s),
            ApiError::Tam(TamError::CapabilityDenied(s)) => {
                (StatusCode::FORBIDDEN, format!("capability denied: {s}"))
            }
            ApiError::Tam(TamError::BudgetExceeded { need, have }) => (
                StatusCode::TOO_MANY_REQUESTS,
                format!("attention budget exceeded: need {need}, have {have}"),
            ),
            ApiError::Tam(TamError::NotFound(s)) => (StatusCode::NOT_FOUND, s),
            ApiError::Tam(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

// ---------------------------------------------------------------- requests/responses

#[derive(Deserialize)]
pub struct SpawnReq {
    pub id: String,
    #[serde(default = "default_budget")]
    pub budget: u64,
}
fn default_budget() -> u64 {
    100_000
}

#[derive(Serialize)]
pub struct StatusResp {
    pub id: String,
    pub phase: String,
    pub remaining_budget: u64,
    pub audit_count: usize,
}

#[derive(Deserialize)]
pub struct ThinkReq {
    pub prompt: String,
    #[serde(default = "default_think_cost")]
    pub cost: u64,
}
fn default_think_cost() -> u64 {
    500
}

#[derive(Serialize)]
pub struct ThinkResp {
    pub content: String,
    pub tokens: u64,
    pub remaining_budget: u64,
}

#[derive(Deserialize)]
pub struct RememberReq {
    pub id: String,
    pub vector: Vec<f32>,
    #[serde(default)]
    pub data: String,
    #[serde(default = "default_small_cost")]
    pub cost: u64,
}
fn default_small_cost() -> u64 {
    5
}

#[derive(Deserialize)]
pub struct RecallReq {
    pub query: Vec<f32>,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default = "default_small_cost")]
    pub cost: u64,
}
fn default_k() -> usize {
    3
}

#[derive(Serialize)]
pub struct Hit {
    pub id: String,
    pub data: String,
}

#[derive(Deserialize)]
pub struct InvokeReq {
    pub tool: String,
    pub input: String,
    #[serde(default = "default_invoke_cost")]
    pub cost: u64,
}
fn default_invoke_cost() -> u64 {
    200
}

#[derive(Serialize)]
pub struct OutputResp {
    pub output: String,
    pub remaining_budget: u64,
}

#[derive(Serialize)]
pub struct AuditEntry {
    pub op: String,
    pub permission: Option<String>,
    pub cost: u64,
    pub target: String,
    pub allowed: bool,
}

// ---------------------------------------------------------------- router

/// Build the gateway router over shared state.
pub fn build_router(state: Arc<GatewayState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/cluster", get(cluster))
        .route("/agents", post(spawn))
        .route("/agents/{id}", get(status))
        .route("/agents/{id}/think", post(think))
        .route("/agents/{id}/remember", post(remember))
        .route("/agents/{id}/recall", post(recall))
        .route("/agents/{id}/invoke", post(invoke))
        .route("/agents/{id}/audit", get(audit))
        .route("/agents/{id}/events", get(events))
        .with_state(state)
}

/// Bind and serve the gateway (blocks until shutdown).
pub async fn serve(state: Arc<GatewayState>, addr: &str) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("THALIOX gateway on http://{addr}");
    axum::serve(listener, build_router(state)).await
}

// ---------------------------------------------------------------- handlers

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "thaliox-gateway" }))
}

fn mem_cap(id: &str, perm: Permission, pattern: String) -> CapabilityToken {
    CapabilityToken {
        subject: AgentId::new(id),
        permissions: vec![perm],
        scope: vec![Scope {
            resource: ResourceKind::Memory,
            pattern,
        }],
        issued_at: 0,
        expires_at: 0,
        jti: [0; 16],
        delegable: false,
        signature: [0; 32],
    }
}

fn tool_cap(id: &str) -> CapabilityToken {
    CapabilityToken {
        subject: AgentId::new(id),
        permissions: vec![Permission::Execute],
        scope: vec![Scope {
            resource: ResourceKind::Tool,
            pattern: "tool://*".into(),
        }],
        issued_at: 0,
        expires_at: 0,
        jti: [0; 16],
        delegable: false,
        signature: [0; 32],
    }
}

fn status_of(a: &Agent, id: &str) -> StatusResp {
    StatusResp {
        id: id.to_string(),
        phase: format!("{:?}", a.phase()),
        remaining_budget: a.remaining_budget(),
        audit_count: a.audit().len(),
    }
}

async fn spawn(
    State(st): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(req): Json<SpawnReq>,
) -> Result<Json<StatusResp>, ApiError> {
    st.admit(&headers, Permission::Communicate, &req.id)?;
    let mut agent = Agent::new(
        AgentId::new(&req.id),
        AttentionBudget::new(req.budget, 1_000_000),
        st.memory.clone(),
        st.mind.clone(),
    );
    for t in &st.tools {
        agent = agent.with_tool(t.clone());
    }
    agent = agent
        .grant(mem_cap(
            &req.id,
            Permission::Write,
            format!("mem://{}/*", req.id),
        ))
        .grant(mem_cap(
            &req.id,
            Permission::Read,
            format!("mem://{}/*", req.id),
        ))
        .grant(tool_cap(&req.id));
    agent.start()?;
    let resp = status_of(&agent, &req.id);
    st.agents
        .lock()
        .unwrap()
        .insert(req.id.clone(), Arc::new(AsyncMutex::new(agent)));
    Ok(Json(resp))
}

async fn status(
    State(st): State<Arc<GatewayState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<StatusResp>, ApiError> {
    st.admit(&headers, Permission::Communicate, &id)?;
    let a = match st.locate(&id, "")? {
        Located::Local(a) => a,
        Located::Remote(url) => return Err(ApiError::Located(url)),
    };
    let a = a.lock().await;
    Ok(Json(status_of(&a, &id)))
}

async fn think(
    State(st): State<Arc<GatewayState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<ThinkReq>,
) -> Result<Json<ThinkResp>, ApiError> {
    st.admit(&headers, Permission::Communicate, &id)?;
    let a = match st.locate(&id, "/think")? {
        Located::Local(a) => a,
        Located::Remote(url) => return Err(ApiError::Located(url)),
    };
    let mut a = a.lock().await;
    match a
        .act(Action::Think {
            prompt: req.prompt,
            cost: req.cost,
        })
        .await?
    {
        Outcome::Thought(c) => Ok(Json(ThinkResp {
            content: c.content,
            tokens: c.tokens,
            remaining_budget: a.remaining_budget(),
        })),
        _ => Err(ApiError::Tam(TamError::Invalid(
            "unexpected outcome".into(),
        ))),
    }
}

async fn remember(
    State(st): State<Arc<GatewayState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<RememberReq>,
) -> Result<Json<StatusResp>, ApiError> {
    st.admit(&headers, Permission::Communicate, &id)?;
    let a = match st.locate(&id, "/remember")? {
        Located::Local(a) => a,
        Located::Remote(url) => return Err(ApiError::Located(url)),
    };
    let mut a = a.lock().await;
    let obj = SemanticObject {
        id: req.id,
        vector: req.vector,
        tags: vec![],
        data: req.data.into_bytes(),
        capability: None,
    };
    a.act(Action::Remember {
        object: obj,
        cost: req.cost,
    })
    .await?;
    Ok(Json(status_of(&a, &id)))
}

async fn recall(
    State(st): State<Arc<GatewayState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<RecallReq>,
) -> Result<Json<Vec<Hit>>, ApiError> {
    st.admit(&headers, Permission::Communicate, &id)?;
    let a = match st.locate(&id, "/recall")? {
        Located::Local(a) => a,
        Located::Remote(url) => return Err(ApiError::Located(url)),
    };
    let mut a = a.lock().await;
    match a
        .act(Action::Recall {
            query: req.query,
            k: req.k,
            cost: req.cost,
        })
        .await?
    {
        Outcome::Recalled(hits) => Ok(Json(
            hits.into_iter()
                .map(|o| Hit {
                    id: o.id,
                    data: String::from_utf8_lossy(&o.data).into_owned(),
                })
                .collect(),
        )),
        _ => Err(ApiError::Tam(TamError::Invalid(
            "unexpected outcome".into(),
        ))),
    }
}

async fn invoke(
    State(st): State<Arc<GatewayState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<InvokeReq>,
) -> Result<Json<OutputResp>, ApiError> {
    st.admit(&headers, Permission::Communicate, &id)?;
    let a = match st.locate(&id, "/invoke")? {
        Located::Local(a) => a,
        Located::Remote(url) => return Err(ApiError::Located(url)),
    };
    let mut a = a.lock().await;
    match a
        .act(Action::Invoke {
            tool: req.tool,
            input: req.input,
            cost: req.cost,
        })
        .await?
    {
        Outcome::Invoked(out) => Ok(Json(OutputResp {
            output: out,
            remaining_budget: a.remaining_budget(),
        })),
        _ => Err(ApiError::Tam(TamError::Invalid(
            "unexpected outcome".into(),
        ))),
    }
}

fn audit_entries(a: &Agent) -> Vec<AuditEntry> {
    a.audit()
        .iter()
        .map(|r| AuditEntry {
            op: format!("{:?}", r.op),
            permission: r.permission_used.map(|p| format!("{p:?}")),
            cost: r.cost,
            target: r.target.clone(),
            allowed: r.allowed,
        })
        .collect()
}

async fn audit(
    State(st): State<Arc<GatewayState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<AuditEntry>>, ApiError> {
    st.admit(&headers, Permission::Communicate, &id)?;
    let a = match st.locate(&id, "/audit")? {
        Located::Local(a) => a,
        Located::Remote(url) => return Err(ApiError::Located(url)),
    };
    let a = a.lock().await;
    Ok(Json(audit_entries(&a)))
}

/// The cluster topology this gateway sees: its node id, peers, and which agents
/// are local vs placed on a peer — proof that the door fronts a fleet.
async fn cluster(State(st): State<Arc<GatewayState>>) -> Json<serde_json::Value> {
    let local: Vec<String> = st.agents.lock().unwrap().keys().cloned().collect();
    let remote = st.directory.lock().unwrap().clone();
    let peers = st.peers.lock().unwrap().clone();
    Json(serde_json::json!({
        "node": st.node.0,
        "require_caps": st.require_caps,
        "local_agents": local,
        "remote_agents": remote,
        "peers": peers,
    }))
}

/// Live I/O surface (Server-Sent Events): stream the agent's audit log as
/// `audit` events. A finite snapshot today; a live tail (broadcast) is the next
/// increment. Goes through the same admission + routing as every other surface.
async fn events(
    State(st): State<Arc<GatewayState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, axum::Error>>>, ApiError> {
    st.admit(&headers, Permission::Communicate, &id)?;
    let a = match st.locate(&id, "/events")? {
        Located::Local(a) => a,
        Located::Remote(url) => return Err(ApiError::Located(url)),
    };
    let a = a.lock().await;
    let events: Vec<Result<Event, axum::Error>> = audit_entries(&a)
        .iter()
        .map(|e| Event::default().event("audit").json_data(e))
        .collect();
    Ok(Sse::new(tokio_stream::iter(events)))
}
