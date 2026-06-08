//! # fabric-node — a cluster node binary (M4b cross-host validation)
//!
//! Runs as a migration **server** or **client** so an agent can migrate between
//! two real machines over TCP (RFC-0006 §3). Static-musl + no remote backends,
//! so it ships to a host without a Rust toolchain.
//!
//!   fabric-node serve   <bind_addr>    # receive a migration, print the agent
//!   fabric-node migrate <server_addr>  # send a demo agent (budget 100→95) over

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thaliox_cognition::MockProvider;
use thaliox_core::{AgentId, AttentionBudget};
use thaliox_fabric::{send_migration, serve_migrations};
use thaliox_memory::InMemorySpace;
use thaliox_runtime::{Action, Agent, DeployEnv, Manifest, Node, Package};

const DEMO_AGENT: &str = "vm-agent";

/// A demo agent that has done one unit of work (budget 100 → 95), packed.
async fn demo_package() -> Package {
    let mut a = Agent::new(
        AgentId::new(DEMO_AGENT),
        AttentionBudget::new(100, 1000),
        Arc::new(InMemorySpace::new()),
        Arc::new(MockProvider::new("ok", 5)),
    );
    a.start().expect("agent starts");
    a.act(Action::Think {
        prompt: "work".into(),
        cost: 5,
    })
    .await
    .expect("think");
    Package::pack(&a, Manifest::new(AgentId::new(DEMO_AGENT)))
}

fn guest_env() -> DeployEnv {
    DeployEnv {
        memory: Arc::new(InMemorySpace::new()),
        mind: Arc::new(MockProvider::new("ok", 5)),
        tools: vec![],
        verifier: None,
    }
}

async fn serve(bind: SocketAddr) -> i32 {
    let node = Arc::new(Mutex::new(Node::new("remote")));
    let bound = match serve_migrations(node.clone(), guest_env, bind).await {
        Ok(a) => a,
        Err(e) => {
            println!("[node] listen failed: {e}");
            return 1;
        }
    };
    println!("[node] migration server listening on {bound}");

    // Wait for an agent to arrive, then report its (migrated) state.
    let id = AgentId::new(DEMO_AGENT);
    for _ in 0..600 {
        if let Some(a) = node.lock().unwrap().agent(&id) {
            println!(
                "[node] received migrated agent {:?}; budget={} (state intact)",
                a.id().0,
                a.remaining_budget()
            );
            return 0;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("[node] timed out waiting for a migration");
    1
}

async fn migrate(server: SocketAddr) -> i32 {
    let pkg = demo_package().await;
    match send_migration(server, &pkg).await {
        Ok(()) => {
            println!("[client] migrated agent {DEMO_AGENT:?} to {server} — accepted");
            0
        }
        Err(e) => {
            println!("[client] migration failed: {e}");
            1
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let parse = |s: Option<&String>| -> Option<SocketAddr> { s.and_then(|s| s.parse().ok()) };

    let code = match args.get(1).map(String::as_str) {
        Some("serve") => match parse(args.get(2)) {
            Some(a) => serve(a).await,
            None => {
                eprintln!("usage: fabric-node serve <bind_addr>");
                2
            }
        },
        Some("migrate") => match parse(args.get(2)) {
            Some(a) => migrate(a).await,
            None => {
                eprintln!("usage: fabric-node migrate <server_addr>");
                2
            }
        },
        _ => {
            eprintln!("usage: fabric-node serve <bind_addr> | migrate <server_addr>");
            2
        }
    };
    std::process::exit(code);
}
