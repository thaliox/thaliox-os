//! Gateway integration tests — drive the router via `oneshot` (no real server).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use thaliox_api::{GatewayState, build_router};
use thaliox_cognition::MockProvider;
use thaliox_core::{AgentId, CapabilityToken, Permission, ResourceKind, Scope};
use thaliox_memory::InMemorySpace;

async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let body = match body {
        Some(j) => Body::from(serde_json::to_vec(&j).unwrap()),
        None => Body::empty(),
    };
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, v)
}

#[tokio::test]
async fn spawn_think_audit_flow() {
    let state = GatewayState::new(
        Arc::new(InMemorySpace::new()),
        Arc::new(MockProvider::new("hello from agent", 7)),
        vec![],
    );
    let router = build_router(state);

    // health
    let (s, v) = call(&router, "GET", "/health", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "ok");

    // spawn
    let (s, v) = call(
        &router,
        "POST",
        "/agents",
        Some(json!({"id": "a1", "budget": 1000})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["id"], "a1");
    assert_eq!(v["phase"], "Live");
    assert_eq!(v["remaining_budget"], 1000);

    // think — budget reconciles to the real 7 tokens
    let (s, v) = call(
        &router,
        "POST",
        "/agents/a1/think",
        Some(json!({"prompt": "hi", "cost": 5})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["content"], "hello from agent");
    assert_eq!(v["tokens"], 7);
    assert_eq!(v["remaining_budget"], 993);

    // remember + recall round-trip through HTTP
    let (s, _) = call(
        &router,
        "POST",
        "/agents/a1/remember",
        Some(json!({"id": "note-1", "vector": [1.0, 0.0], "data": "hello", "cost": 3})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = call(
        &router,
        "POST",
        "/agents/a1/recall",
        Some(json!({"query": [0.9, 0.1], "k": 1, "cost": 2})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v[0]["id"], "note-1");
    assert_eq!(v[0]["data"], "hello");

    // audit lists the three calls
    let (s, v) = call(&router, "GET", "/agents/a1/audit", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v.as_array().unwrap().len(), 3);
    assert_eq!(v[0]["op"], "Think");
    assert_eq!(v[0]["allowed"], true);

    // unknown agent → 404
    let (s, _) = call(&router, "GET", "/agents/ghost", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

// ---------- M4d: cluster front door ----------

/// Like `call`, but lets a test set request headers and inspect the response
/// headers + raw body (needed for capability admission, 307s, and SSE).
async fn call_full(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let body = match body {
        Some(j) => Body::from(serde_json::to_vec(&j).unwrap()),
        None => Body::empty(),
    };
    let mut rb = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    for (k, v) in headers {
        rb = rb.header(*k, *v);
    }
    let resp = router
        .clone()
        .oneshot(rb.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let hdrs = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, hdrs, bytes)
}

fn comm_cap_json(subject: &str, pattern: &str) -> String {
    let cap = CapabilityToken {
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
    };
    serde_json::to_string(&cap).unwrap()
}

#[tokio::test]
async fn cluster_mode_requires_capability() {
    let state = GatewayState::cluster(
        Arc::new(InMemorySpace::new()),
        Arc::new(MockProvider::new("ok", 5)),
        vec![],
        "A",
        true, // require capabilities at the door
    );
    let router = build_router(state);
    let cap = comm_cap_json("client", "a1");

    // spawn without a capability → 403
    let (s, _, _) = call_full(&router, "POST", "/agents", Some(json!({"id": "a1"})), &[]).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // spawn with the right capability → 200
    let (s, _, _) = call_full(
        &router,
        "POST",
        "/agents",
        Some(json!({"id": "a1", "budget": 1000})),
        &[("x-thaliox-capability", &cap)],
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // think with the capability → 200
    let (s, _, _) = call_full(
        &router,
        "POST",
        "/agents/a1/think",
        Some(json!({"prompt": "hi", "cost": 5})),
        &[("x-thaliox-capability", &cap)],
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // think without a capability → 403
    let (s, _, _) = call_full(
        &router,
        "POST",
        "/agents/a1/think",
        Some(json!({"prompt": "hi"})),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // a capability scoped to another agent → 403
    let wrong = comm_cap_json("client", "other");
    let (s, _, _) = call_full(
        &router,
        "POST",
        "/agents/a1/think",
        Some(json!({"prompt": "hi"})),
        &[("x-thaliox-capability", &wrong)],
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cluster_routing_redirects_to_peer() {
    let state = GatewayState::cluster(
        Arc::new(InMemorySpace::new()),
        Arc::new(MockProvider::new("ok", 5)),
        vec![],
        "A",
        false,
    );
    state.register_peer("B", "http://node-b:8088");
    state.place_remote("rem", "B");
    let router = build_router(state);

    // A read for a peer-hosted agent → 307 to node B's gateway.
    let (s, h, _) = call_full(&router, "GET", "/agents/rem", None, &[]).await;
    assert_eq!(s, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(h.get("location").unwrap(), "http://node-b:8088/agents/rem");

    // A driving op redirects with the operation sub-path preserved.
    let (s, h, _) = call_full(
        &router,
        "POST",
        "/agents/rem/think",
        Some(json!({"prompt": "x"})),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        h.get("location").unwrap(),
        "http://node-b:8088/agents/rem/think"
    );
}

#[tokio::test]
async fn events_streams_audit_over_sse() {
    let state = GatewayState::new(
        Arc::new(InMemorySpace::new()),
        Arc::new(MockProvider::new("hello", 7)),
        vec![],
    );
    let router = build_router(state);
    call_full(
        &router,
        "POST",
        "/agents",
        Some(json!({"id": "a1", "budget": 1000})),
        &[],
    )
    .await;
    call_full(
        &router,
        "POST",
        "/agents/a1/think",
        Some(json!({"prompt": "hi", "cost": 5})),
        &[],
    )
    .await;

    let (s, h, body) = call_full(&router, "GET", "/agents/a1/events", None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        h.get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("event: audit"));
    assert!(text.contains("Think")); // the streamed audit entry
}

#[tokio::test]
async fn cluster_endpoint_reports_topology() {
    let state = GatewayState::cluster(
        Arc::new(InMemorySpace::new()),
        Arc::new(MockProvider::new("ok", 5)),
        vec![],
        "A",
        false,
    );
    state.register_peer("B", "http://node-b:8088");
    state.place_remote("rem", "B");
    let router = build_router(state);
    call_full(
        &router,
        "POST",
        "/agents",
        Some(json!({"id": "loc", "budget": 100})),
        &[],
    )
    .await;

    let (s, _, body) = call_full(&router, "GET", "/cluster", None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["node"], "A");
    assert_eq!(v["local_agents"][0], "loc");
    assert_eq!(v["remote_agents"]["rem"], "B");
    assert_eq!(v["peers"]["B"], "http://node-b:8088");
}
