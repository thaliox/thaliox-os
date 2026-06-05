//! # THALIOX api (L5) — the unified HTTP gateway
//!
//! An axum service that maps external requests onto the agent runtime: spawn an
//! agent, then drive it (`think` / `remember` / `recall` / `invoke`) and read
//! its `audit`. Every request bottoms out in [`Agent::act`](thaliox_runtime::Agent)
//! — so the TAM gates (INV-1 budget · INV-2 capability · INV-4 audit) apply to
//! HTTP callers too. (MASTER_PLAN §3 item 11, F6.)
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
use axum::http::StatusCode;
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
use thaliox_runtime::{Action, Agent, Outcome};

/// Shared gateway state: a memory, a cognition backend, a tool set, and the live
/// agent table. Each spawned agent is wrapped in an async mutex (its `act` is
/// `&mut`); the table itself is a plain mutex held only briefly.
pub struct GatewayState {
    memory: Arc<dyn SemanticSpace>,
    mind: Arc<dyn LlmProvider>,
    tools: Vec<Arc<dyn Tool>>,
    agents: Mutex<HashMap<String, Arc<AsyncMutex<Agent>>>>,
}

impl GatewayState {
    /// Build shared state over a memory, cognition backend, and tool set.
    pub fn new(
        memory: Arc<dyn SemanticSpace>,
        mind: Arc<dyn LlmProvider>,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            memory,
            mind,
            tools,
            agents: Mutex::new(HashMap::new()),
        })
    }

    fn agent(&self, id: &str) -> Result<Arc<AsyncMutex<Agent>>, ApiError> {
        self.agents
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::NotFound(format!("agent '{id}'")))
    }
}

/// Maps a [`TamError`] / not-found onto an HTTP status + JSON body.
pub enum ApiError {
    NotFound(String),
    Tam(TamError),
}

impl From<TamError> for ApiError {
    fn from(e: TamError) -> Self {
        ApiError::Tam(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
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
        .route("/agents", post(spawn))
        .route("/agents/{id}", get(status))
        .route("/agents/{id}/think", post(think))
        .route("/agents/{id}/remember", post(remember))
        .route("/agents/{id}/recall", post(recall))
        .route("/agents/{id}/invoke", post(invoke))
        .route("/agents/{id}/audit", get(audit))
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
    Json(req): Json<SpawnReq>,
) -> Result<Json<StatusResp>, ApiError> {
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
) -> Result<Json<StatusResp>, ApiError> {
    let a = st.agent(&id)?;
    let a = a.lock().await;
    Ok(Json(status_of(&a, &id)))
}

async fn think(
    State(st): State<Arc<GatewayState>>,
    Path(id): Path<String>,
    Json(req): Json<ThinkReq>,
) -> Result<Json<ThinkResp>, ApiError> {
    let a = st.agent(&id)?;
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
    Json(req): Json<RememberReq>,
) -> Result<Json<StatusResp>, ApiError> {
    let a = st.agent(&id)?;
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
    Json(req): Json<RecallReq>,
) -> Result<Json<Vec<Hit>>, ApiError> {
    let a = st.agent(&id)?;
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
    Json(req): Json<InvokeReq>,
) -> Result<Json<OutputResp>, ApiError> {
    let a = st.agent(&id)?;
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

async fn audit(
    State(st): State<Arc<GatewayState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<AuditEntry>>, ApiError> {
    let a = st.agent(&id)?;
    let a = a.lock().await;
    let entries = a
        .audit()
        .iter()
        .map(|r| AuditEntry {
            op: format!("{:?}", r.op),
            permission: r.permission_used.map(|p| format!("{p:?}")),
            cost: r.cost,
            target: r.target.clone(),
            allowed: r.allowed,
        })
        .collect();
    Ok(Json(entries))
}
