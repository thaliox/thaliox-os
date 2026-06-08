//! # THALIOX guest agent-runner (RFC-0004 F2a)
//!
//! Runs **inside** a Firecracker microVM: read a [`Package`] from the
//! config-drive (`/dev/vdb`), deploy the agent in-VM with `LocalDeploy`, run it,
//! and (as PID 1) power the VM off so Firecracker exits.
//!
//! Two modes in one binary:
//! - `thaliox-runner mkdrive <out>` — **host** side: pack a demo agent into a
//!   config-drive image (length-prefixed [`Package`], padded to a block size).
//! - `thaliox-runner [device]` — **guest** side (default device `/dev/vdb`):
//!   read the package, `LocalDeploy`, run the agent.
//!
//! F2a uses the one-way config-drive channel; F2b swaps it for vsock so the
//! runner can stream health / checkpoints back (RFC-0004 §4).

use std::io::Write;
use std::sync::Arc;

use thaliox_cognition::MockProvider;
use thaliox_core::{AgentId, AttentionBudget};
use thaliox_memory::InMemorySpace;
use thaliox_runtime::{
    Action, Agent, DeployEnv, DeployTarget, LocalDeploy, Manifest, Outcome, Package,
};

/// Host side: build a demo agent, pack it, write a padded config-drive image.
fn make_drive(out: &str) -> i32 {
    let mut agent = Agent::new(
        AgentId::new("vm-agent"),
        AttentionBudget::new(100, 1000),
        Arc::new(InMemorySpace::new()),
        Arc::new(MockProvider::new("ok", 5)),
    );
    agent.start().expect("agent starts");

    let manifest = Manifest::new(AgentId::new("vm-agent")).expecting_model("local-mock");
    let pkg = Package::pack(&agent, manifest);

    let mut img = pkg.to_config_drive();
    let payload = img.len();
    let min = 1usize << 20; // pad to 1 MiB so it looks like a normal block device
    if img.len() < min {
        img.resize(min, 0);
    }
    match std::fs::write(out, &img) {
        Ok(()) => {
            println!(
                "wrote config-drive {out} ({} bytes, payload {payload})",
                img.len()
            );
            0
        }
        Err(e) => {
            eprintln!("write failed: {e}");
            1
        }
    }
}

/// Guest side: read the package, deploy in-VM, run the agent.
async fn run_guest(device: &str) -> i32 {
    println!("[thaliox-runner] reading config-drive {device}");
    let bytes = match std::fs::read(device) {
        Ok(b) => b,
        Err(e) => {
            println!("[thaliox-runner] read failed: {e}");
            return 1;
        }
    };
    let pkg = match Package::from_config_drive(&bytes) {
        Ok(p) => p,
        Err(e) => {
            println!("[thaliox-runner] package parse failed: {e}");
            return 1;
        }
    };
    println!(
        "[thaliox-runner] package for agent {:?}, model {:?}",
        pkg.manifest.agent.0, pkg.manifest.model_fingerprint
    );

    let env = DeployEnv {
        memory: Arc::new(InMemorySpace::new()),
        mind: Arc::new(MockProvider::new("ok", 5)),
        tools: vec![],
        verifier: None,
    };
    let mut agent = match LocalDeploy.deploy(&pkg, env) {
        Ok(a) => a,
        Err(e) => {
            println!("[thaliox-runner] deploy failed: {e}");
            return 1;
        }
    };
    println!(
        "[thaliox-runner] deployed in-VM; phase={:?} budget={}",
        agent.phase(),
        agent.remaining_budget()
    );

    match agent
        .act(Action::Think {
            prompt: "hello from inside the microVM".into(),
            cost: 10,
        })
        .await
    {
        Ok(Outcome::Thought(c)) => println!(
            "[thaliox-runner] agent thought: {:?} ({} tokens); budget now {}",
            c.content,
            c.tokens,
            agent.remaining_budget()
        ),
        Ok(_) => println!("[thaliox-runner] agent acted (non-think outcome)"),
        Err(e) => {
            println!("[thaliox-runner] act failed: {e}");
            return 1;
        }
    }

    // Mint a fresh checkpoint in-VM — foreshadows the F2b vsock checkpoint pull.
    let cp = agent.checkpoint();
    println!(
        "[thaliox-runner] re-checkpoint ok: {} bytes",
        cp.state.len()
    );
    println!("[thaliox-runner] OK — agent ran inside the microVM");
    0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let code = if args.get(1).map(String::as_str) == Some("mkdrive") {
        make_drive(args.get(2).map(String::as_str).unwrap_or("package.img"))
    } else {
        let device = args
            .get(1)
            .cloned()
            .unwrap_or_else(|| "/dev/vdb".to_string());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(run_guest(&device))
    };

    std::io::stdout().flush().ok();

    // As PID 1 in the guest, returning would panic the kernel. Reset the VM
    // (RB_AUTOBOOT + the `reboot=k` boot arg → i8042 reset) so Firecracker exits
    // cleanly — F3 relies on detecting that exit. On a normal host (pid != 1)
    // just exit, so running this binary outside a VM never resets the host.
    if std::process::id() == 1 {
        unsafe {
            libc::sync();
            libc::reboot(libc::RB_AUTOBOOT);
        }
        // If reboot returned (shouldn't as PID 1), avoid exiting PID 1.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
    std::process::exit(code);
}
