//! # THALIOX guest agent-runner (RFC-0004 F2a/F2b)
//!
//! Runs **inside** a Firecracker microVM: deploy an agent from a [`Package`]
//! with `LocalDeploy`, run it, and serve control requests.
//!
//! Modes (one binary):
//! - **guest, vsock (F2b, default-in-VM)** `serve [port]` — listen on `AF_VSOCK`
//!   and serve `Deploy` / `Health` / `Checkpoint` / `Shutdown` over a tiny framed
//!   protocol. Bidirectional: the host sends a `Package` in and pulls a fresh
//!   checkpoint back.
//! - **host, vsock client (F2b)** `host <uds> [port]` — drive the sequence over
//!   Firecracker's vsock UDS (deploy → health → checkpoint → shutdown).
//! - **guest, config-drive (F2a)** `drive [device]` — read a `Package` from
//!   `/dev/vdb`, deploy, run once.
//! - **host helper (F2a)** `mkdrive <out>` — write a padded config-drive image.
//!
//! As PID 1 in the guest, the runner resets the VM on exit (RB_AUTOBOOT +
//! `reboot=k`) so Firecracker exits cleanly; on a normal host it just exits.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use thaliox_cognition::MockProvider;
use thaliox_core::{AgentId, AttentionBudget};
use thaliox_memory::InMemorySpace;
use thaliox_runtime::{
    Action, Agent, DeployEnv, DeployTarget, LocalDeploy, Manifest, Outcome, Package,
};

/// Default vsock port the guest runner listens on.
const DEFAULT_PORT: u32 = 1024;

// ----- shared framing: [op: u8][len: u64 LE][payload] -----

mod op {
    pub const DEPLOY: u8 = 1;
    pub const HEALTH: u8 = 2;
    pub const CHECKPOINT: u8 = 3;
    pub const SHUTDOWN: u8 = 4;
    pub const OK: u8 = 0;
    pub const ERR: u8 = 1;
}

fn write_frame<W: Write>(w: &mut W, tag: u8, payload: &[u8]) -> io::Result<()> {
    w.write_all(&[tag])?;
    w.write_all(&(payload.len() as u64).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

fn read_frame<R: Read>(r: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let mut tag = [0u8; 1];
    r.read_exact(&mut tag)?;
    let mut len = [0u8; 8];
    r.read_exact(&mut len)?;
    let mut buf = vec![0u8; u64::from_le_bytes(len) as usize];
    r.read_exact(&mut buf)?;
    Ok((tag[0], buf))
}

// ----- the demo agent / package (shared by host helpers) -----

fn demo_package() -> Package {
    let mut agent = Agent::new(
        AgentId::new("vm-agent"),
        AttentionBudget::new(100, 1000),
        Arc::new(InMemorySpace::new()),
        Arc::new(MockProvider::new("ok", 5)),
    );
    agent.start().expect("agent starts");
    Package::pack(
        &agent,
        Manifest::new(AgentId::new("vm-agent")).expecting_model("local-mock"),
    )
}

fn guest_env() -> DeployEnv {
    DeployEnv {
        memory: Arc::new(InMemorySpace::new()),
        mind: Arc::new(MockProvider::new("ok", 5)),
        tools: vec![],
        verifier: None,
    }
}

// ----- guest: vsock server (F2b) -----

/// Listen on `AF_VSOCK` (any CID) at `port`, returning the listening fd.
fn vsock_listen(port: u32) -> io::Result<RawFd> {
    unsafe {
        let fd = libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut addr: libc::sockaddr_vm = std::mem::zeroed();
        addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        addr.svm_port = port;
        addr.svm_cid = libc::VMADDR_CID_ANY;
        let len = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
        if libc::bind(fd, &addr as *const _ as *const libc::sockaddr, len) < 0
            || libc::listen(fd, 8) < 0
        {
            let e = io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        Ok(fd)
    }
}

fn vsock_accept(listen_fd: RawFd) -> io::Result<File> {
    let cfd = unsafe { libc::accept(listen_fd, std::ptr::null_mut(), std::ptr::null_mut()) };
    if cfd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(cfd) })
}

/// Guest vsock server: one request per connection, agent state persists across
/// connections in the accept loop.
fn serve(port: u32) -> i32 {
    println!("[thaliox-runner] vsock serve on port {port}");
    let listen_fd = match vsock_listen(port) {
        Ok(f) => f,
        Err(e) => {
            println!("[thaliox-runner] vsock listen failed: {e}");
            return 1;
        }
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let mut agent: Option<Agent> = None;

    loop {
        let mut conn = match vsock_accept(listen_fd) {
            Ok(c) => c,
            Err(e) => {
                println!("[thaliox-runner] accept failed: {e}");
                continue;
            }
        };
        let (tag, payload) = match read_frame(&mut conn) {
            Ok(x) => x,
            Err(e) => {
                println!("[thaliox-runner] read failed: {e}");
                continue;
            }
        };
        match tag {
            op::DEPLOY => match Package::from_bytes(&payload) {
                Ok(pkg) => match LocalDeploy.deploy(&pkg, guest_env()) {
                    Ok(mut a) => {
                        let _ = rt.block_on(a.act(Action::Think {
                            prompt: "hello over vsock".into(),
                            cost: 10,
                        }));
                        let msg = format!(
                            "deployed agent {:?}; phase={:?} budget={}",
                            a.id().0,
                            a.phase(),
                            a.remaining_budget()
                        );
                        println!("[thaliox-runner] {msg}");
                        agent = Some(a);
                        let _ = write_frame(&mut conn, op::OK, msg.as_bytes());
                    }
                    Err(e) => {
                        let _ = write_frame(&mut conn, op::ERR, format!("deploy: {e}").as_bytes());
                    }
                },
                Err(e) => {
                    let _ = write_frame(&mut conn, op::ERR, format!("package: {e}").as_bytes());
                }
            },
            op::HEALTH => {
                let msg = match &agent {
                    Some(a) => {
                        format!("live phase={:?} budget={}", a.phase(), a.remaining_budget())
                    }
                    None => "no agent deployed".to_string(),
                };
                let _ = write_frame(&mut conn, op::OK, msg.as_bytes());
            }
            op::CHECKPOINT => match &agent {
                Some(a) => {
                    let pkg = Package::pack(
                        a,
                        Manifest::new(a.id().clone()).expecting_model("local-mock"),
                    );
                    let _ = write_frame(&mut conn, op::OK, &pkg.to_bytes());
                }
                None => {
                    let _ = write_frame(&mut conn, op::ERR, b"no agent");
                }
            },
            op::SHUTDOWN => {
                let _ = write_frame(&mut conn, op::OK, b"bye");
                println!("[thaliox-runner] shutdown requested");
                break;
            }
            other => {
                let _ = write_frame(&mut conn, op::ERR, format!("unknown op {other}").as_bytes());
            }
        }
    }
    unsafe {
        libc::close(listen_fd);
    }
    0
}

// ----- host: vsock client over Firecracker's UDS (F2b) -----

/// One request/response over Firecracker's vsock: connect the UDS, `CONNECT
/// <port>`, then exchange one frame.
fn host_rpc(uds: &str, port: u32, tag: u8, payload: &[u8]) -> io::Result<(u8, Vec<u8>)> {
    let mut s = UnixStream::connect(uds)?;
    s.write_all(format!("CONNECT {port}\n").as_bytes())?;
    // Firecracker replies "OK <hostside_port>\n" once the guest accepts.
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        s.read_exact(&mut b)?;
        if b[0] == b'\n' {
            break;
        }
        line.push(b[0]);
    }
    if !line.starts_with(b"OK") {
        return Err(io::Error::other(format!(
            "vsock CONNECT refused: {}",
            String::from_utf8_lossy(&line)
        )));
    }
    write_frame(&mut s, tag, payload)?;
    read_frame(&mut s)
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Host side: drive the full bidirectional sequence against a serving guest.
fn host_drive(uds: &str, port: u32) -> i32 {
    let pkg = demo_package();

    // Deploy — retry while the guest finishes booting and starts listening.
    let mut deployed = false;
    for attempt in 0..40 {
        match host_rpc(uds, port, op::DEPLOY, &pkg.to_bytes()) {
            Ok((st, resp)) => {
                println!("deploy     -> [{st}] {}", lossy(&resp));
                deployed = true;
                break;
            }
            Err(e) => {
                if attempt == 39 {
                    println!("deploy failed after retries: {e}");
                    return 1;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    if !deployed {
        return 1;
    }

    match host_rpc(uds, port, op::HEALTH, &[]) {
        Ok((st, r)) => println!("health     -> [{st}] {}", lossy(&r)),
        Err(e) => println!("health err: {e}"),
    }

    match host_rpc(uds, port, op::CHECKPOINT, &[]) {
        Ok((st, r)) => match Package::from_bytes(&r) {
            Ok(p) => println!(
                "checkpoint -> [{st}] pulled package for {:?} ({} bytes)",
                p.manifest.agent.0,
                r.len()
            ),
            Err(e) => println!("checkpoint parse err: {e}"),
        },
        Err(e) => println!("checkpoint err: {e}"),
    }

    match host_rpc(uds, port, op::SHUTDOWN, &[]) {
        Ok((st, r)) => println!("shutdown   -> [{st}] {}", lossy(&r)),
        Err(e) => println!("shutdown err: {e}"),
    }
    0
}

// ----- guest: config-drive (F2a) -----

fn run_drive(device: &str) -> i32 {
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
    let mut agent = match LocalDeploy.deploy(&pkg, guest_env()) {
        Ok(a) => a,
        Err(e) => {
            println!("[thaliox-runner] deploy failed: {e}");
            return 1;
        }
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(agent.act(Action::Think {
        prompt: "hello from inside the microVM".into(),
        cost: 10,
    })) {
        Ok(Outcome::Thought(c)) => println!(
            "[thaliox-runner] agent thought: {:?}; budget now {}",
            c.content,
            agent.remaining_budget()
        ),
        Ok(_) => println!("[thaliox-runner] agent acted"),
        Err(e) => {
            println!("[thaliox-runner] act failed: {e}");
            return 1;
        }
    }
    println!("[thaliox-runner] OK — agent ran inside the microVM");
    0
}

// ----- host helper: write a config-drive image (F2a) -----

fn make_drive(out: &str) -> i32 {
    let pkg = demo_package();
    let mut img = pkg.to_config_drive();
    let payload = img.len();
    let min = 1usize << 20;
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let parse_port = |i: usize| -> u32 {
        args.get(i)
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_PORT)
    };

    let code = match args.get(1).map(String::as_str) {
        Some("serve") => serve(parse_port(2)),
        Some("host") => host_drive(
            args.get(2)
                .map(String::as_str)
                .unwrap_or("/tmp/fc-vsock.sock"),
            parse_port(3),
        ),
        Some("drive") => run_drive(args.get(2).map(String::as_str).unwrap_or("/dev/vdb")),
        Some("mkdrive") => make_drive(args.get(2).map(String::as_str).unwrap_or("package.img")),
        // Default in-VM (no recognized subcommand): config-drive at /dev/vdb.
        _ => run_drive("/dev/vdb"),
    };

    std::io::stdout().flush().ok();

    // As PID 1 in the guest, returning would panic the kernel. Reset the VM
    // (RB_AUTOBOOT + the `reboot=k` boot arg → i8042 reset) so Firecracker exits
    // cleanly. On a normal host (pid != 1) just exit, so running this binary
    // outside a VM never resets the host.
    if std::process::id() == 1 {
        unsafe {
            libc::sync();
            libc::reboot(libc::RB_AUTOBOOT);
        }
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    std::process::exit(code);
}
