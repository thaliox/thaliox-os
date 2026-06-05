//! Gateway integration tests — drive the router via `oneshot` (no real server).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use thaliox_api::{GatewayState, build_router};
use thaliox_cognition::MockProvider;
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
