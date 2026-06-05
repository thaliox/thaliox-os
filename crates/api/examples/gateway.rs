//! # gateway — start the THALIOX gateway service
//!
//! Starts an axum HTTP service and drives an agent with curl (or any client).
//! Cognition uses a local mock (offline); swap MockProvider for a real
//! thaliox-cognition provider to connect a real model.
//!
//! ```bash
//! cargo run -p thaliox-api --example gateway
//! # In another terminal:
//! curl -s -XPOST localhost:8088/agents -d '{"id":"a1","budget":5000}'
//! curl -s -XPOST localhost:8088/agents/a1/think    -d '{"prompt":"hello","cost":100}'
//! curl -s -XPOST localhost:8088/agents/a1/invoke   -d '{"tool":"fetch","input":"https://example.com","cost":100}'
//! curl -s        localhost:8088/agents/a1/audit
//! ```

use std::sync::Arc;

use thaliox_api::{GatewayState, serve};
use thaliox_cognition::MockProvider;
use thaliox_memory::InMemorySpace;
use thaliox_tools::Fetch;

#[tokio::main]
async fn main() {
    let state = GatewayState::new(
        Arc::new(InMemorySpace::new()),
        Arc::new(MockProvider::new(
            "THALIOX: an operating system built for AI, by AI.",
            8,
        )),
        vec![Arc::new(Fetch::new())],
    );

    println!("THALIOX gateway —— HTTP entry point for the single-node MVP");
    println!("  POST /agents            spawn an agent");
    println!("  POST /agents/{{id}}/think  remember / recall / invoke");
    println!("  GET  /agents/{{id}}/audit  audit log\n");

    serve(state, "127.0.0.1:8088").await.unwrap();
}
