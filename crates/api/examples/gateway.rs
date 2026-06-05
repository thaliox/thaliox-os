//! # gateway — 起 THALIOX 网关服务
//!
//! 起 axum HTTP 服务,用 curl(或任意客户端)驱动一个 agent。cognition 用本地
//! mock(离线);把 MockProvider 换成 thaliox-cognition 的真实 provider 即接真模型。
//!
//! ```bash
//! cargo run -p thaliox-api --example gateway
//! # 另一个终端:
//! curl -s -XPOST localhost:8088/agents -d '{"id":"a1","budget":5000}'
//! curl -s -XPOST localhost:8088/agents/a1/think    -d '{"prompt":"你好","cost":100}'
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
            "THALIOX:为 AI、由 AI 打造的操作系统。",
            8,
        )),
        vec![Arc::new(Fetch::new())],
    );

    println!("THALIOX gateway —— 单机 MVP 的 HTTP 入口");
    println!("  POST /agents            spawn 一个 agent");
    println!("  POST /agents/{{id}}/think  remember / recall / invoke");
    println!("  GET  /agents/{{id}}/audit  审计日志\n");

    serve(state, "127.0.0.1:8088").await.unwrap();
}
