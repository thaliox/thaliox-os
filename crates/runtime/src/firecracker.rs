//! # FirecrackerDeploy — host-side microVM launch target (RFC-0004 F3)
//!
//! Turns the manual API orchestration into a reusable Rust handle. [`launch`]
//! spawns `firecracker`, configures it (kernel + rootfs + vsock) over its API
//! socket, and starts the guest — whose baked-in agent-runner serves on vsock.
//! The returned [`MicroVm`] then drives the agent over that channel: `deploy`
//! (send a [`Package`] in), `health`, `checkpoint` (pull a `Package` back),
//! `shutdown`.
//!
//! Pure `std` (process + Unix sockets + a hand-rolled HTTP/1.1 PUT), behind the
//! `firecracker` feature; it needs a KVM host, so it is self-hosted and out of
//! the default CI gate.
//!
//! [`launch`]: FirecrackerDeploy::launch

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::Package;
use crate::vmproto::{self, op};

/// What can go wrong launching or driving a microVM.
#[derive(Debug)]
pub enum FcError {
    Io(io::Error),
    /// A Firecracker API call returned a non-2xx status.
    Api(String),
    /// The vsock control channel failed or the guest returned an error.
    Vsock(String),
    /// A returned `Package` could not be decoded.
    Package(crate::PackageError),
    /// A wait (boot, child exit, …) exceeded its deadline.
    Timeout(String),
}

impl fmt::Display for FcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FcError::Io(e) => write!(f, "io: {e}"),
            FcError::Api(s) => write!(f, "firecracker api: {s}"),
            FcError::Vsock(s) => write!(f, "vsock: {s}"),
            FcError::Package(e) => write!(f, "package: {e}"),
            FcError::Timeout(s) => write!(f, "timeout: {s}"),
        }
    }
}

impl std::error::Error for FcError {}

impl From<io::Error> for FcError {
    fn from(e: io::Error) -> Self {
        FcError::Io(e)
    }
}

/// How to launch a microVM. The `rootfs` MUST contain the guest agent-runner as
/// `/usr/bin/thaliox-runner` (the boot args set it as init in `serve` mode).
#[derive(Debug, Clone)]
pub struct FirecrackerConfig {
    pub fc_bin: PathBuf,
    pub kernel: PathBuf,
    pub rootfs: PathBuf,
    pub workdir: PathBuf,
    pub vcpus: u32,
    pub mem_mib: u32,
    pub vsock_port: u32,
    pub guest_cid: u32,
}

impl FirecrackerConfig {
    /// Sensible defaults: 2 vCPUs, 512 MiB, vsock port 1024, guest CID 3.
    pub fn new(
        fc_bin: impl Into<PathBuf>,
        kernel: impl Into<PathBuf>,
        rootfs: impl Into<PathBuf>,
        workdir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            fc_bin: fc_bin.into(),
            kernel: kernel.into(),
            rootfs: rootfs.into(),
            workdir: workdir.into(),
            vcpus: 2,
            mem_mib: 512,
            vsock_port: 1024,
            guest_cid: 3,
        }
    }
}

/// A launcher that boots a microVM per [`FirecrackerConfig`].
pub struct FirecrackerDeploy {
    config: FirecrackerConfig,
}

impl FirecrackerDeploy {
    pub fn new(config: FirecrackerConfig) -> Self {
        Self { config }
    }

    /// Spawn Firecracker, configure the VM, and start it. Returns a handle whose
    /// guest is booting into the agent-runner's vsock server.
    pub fn launch(&self) -> Result<MicroVm, FcError> {
        let c = &self.config;
        let api_sock = c.workdir.join("fc-api.sock");
        let vsock_uds = c.workdir.join("fc-vsock.sock");
        let console = c.workdir.join("fc-console.log");
        let _ = fs::remove_file(&api_sock);
        let _ = fs::remove_file(&vsock_uds);

        let log = fs::File::create(&console)?;
        let child = Command::new(&c.fc_bin)
            .arg("--api-sock")
            .arg(&api_sock)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?;

        // The agent-runner is the init in `serve` mode; `--` hands it the args.
        let boot_args = "console=ttyS0 reboot=k panic=1 pci=off random.trust_cpu=on \
             root=/dev/vda rw init=/usr/bin/thaliox-runner -- serve";

        // If any setup step fails, the Drop on `vm` tears down the child.
        let vm = MicroVm {
            child,
            api_sock,
            vsock_uds,
            port: c.vsock_port,
            console,
        };

        wait_for_path(&vm.api_sock, Duration::from_secs(5))?;
        api_put(
            &vm.api_sock,
            "/boot-source",
            &format!(
                "{{\"kernel_image_path\":\"{}\",\"boot_args\":\"{}\"}}",
                c.kernel.display(),
                boot_args
            ),
        )?;
        api_put(
            &vm.api_sock,
            "/drives/rootfs",
            &format!(
                "{{\"drive_id\":\"rootfs\",\"path_on_host\":\"{}\",\"is_root_device\":true,\"is_read_only\":false}}",
                c.rootfs.display()
            ),
        )?;
        api_put(
            &vm.api_sock,
            "/vsock",
            &format!(
                "{{\"guest_cid\":{},\"uds_path\":\"{}\"}}",
                c.guest_cid,
                vm.vsock_uds.display()
            ),
        )?;
        api_put(
            &vm.api_sock,
            "/machine-config",
            &format!(
                "{{\"vcpu_count\":{},\"mem_size_mib\":{}}}",
                c.vcpus, c.mem_mib
            ),
        )?;
        api_put(
            &vm.api_sock,
            "/actions",
            "{\"action_type\":\"InstanceStart\"}",
        )?;
        Ok(vm)
    }
}

/// A running microVM and its control channel.
pub struct MicroVm {
    child: Child,
    api_sock: PathBuf,
    vsock_uds: PathBuf,
    port: u32,
    console: PathBuf,
}

impl MicroVm {
    /// Path of the captured guest serial console (for debugging).
    pub fn console_path(&self) -> &Path {
        &self.console
    }

    /// Firecracker API socket path.
    pub fn api_socket(&self) -> &Path {
        &self.api_sock
    }

    /// Send a `Package` to the guest; the runner deploys and runs the agent.
    /// Retries while the guest finishes booting and starts listening.
    pub fn deploy(&self, package: &Package) -> Result<String, FcError> {
        let body = package.to_bytes();
        let mut attempts = 0;
        loop {
            match self.rpc(op::DEPLOY, &body) {
                Ok((op::OK, r)) => return Ok(String::from_utf8_lossy(&r).into_owned()),
                Ok((_, r)) => return Err(FcError::Vsock(String::from_utf8_lossy(&r).into_owned())),
                Err(e) => {
                    attempts += 1;
                    if attempts >= 40 {
                        return Err(e);
                    }
                    sleep(Duration::from_millis(300));
                }
            }
        }
    }

    /// Ask the guest for the agent's health.
    pub fn health(&self) -> Result<String, FcError> {
        let (st, r) = self.rpc(op::HEALTH, &[])?;
        status_string(st, r)
    }

    /// Pull a fresh `Package` (re-checkpoint) from the in-VM agent.
    pub fn checkpoint(&self) -> Result<Package, FcError> {
        let (st, r) = self.rpc(op::CHECKPOINT, &[])?;
        if st != op::OK {
            return Err(FcError::Vsock(String::from_utf8_lossy(&r).into_owned()));
        }
        Package::from_bytes(&r).map_err(FcError::Package)
    }

    /// Ask the guest to reset; wait for Firecracker to exit (then ensure it).
    pub fn shutdown(mut self) -> Result<(), FcError> {
        let _ = self.rpc(op::SHUTDOWN, &[]);
        let start = Instant::now();
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(10) {
                let _ = self.child.kill();
                return Ok(());
            }
            sleep(Duration::from_millis(100));
        }
    }

    /// One request/response over Firecracker's vsock UDS (host-initiated
    /// `CONNECT <port>` handshake, then one [`vmproto`] frame).
    fn rpc(&self, tag: u8, payload: &[u8]) -> Result<(u8, Vec<u8>), FcError> {
        let mut s = UnixStream::connect(&self.vsock_uds)?;
        s.write_all(format!("CONNECT {}\n", self.port).as_bytes())?;
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
            return Err(FcError::Vsock(format!(
                "CONNECT refused: {}",
                String::from_utf8_lossy(&line)
            )));
        }
        vmproto::write_frame(&mut s, tag, payload)?;
        Ok(vmproto::read_frame(&mut s)?)
    }
}

impl Drop for MicroVm {
    fn drop(&mut self) {
        // Best-effort teardown if the handle is dropped without shutdown().
        let _ = self.child.kill();
    }
}

fn status_string(st: u8, r: Vec<u8>) -> Result<String, FcError> {
    let s = String::from_utf8_lossy(&r).into_owned();
    if st == op::OK {
        Ok(s)
    } else {
        Err(FcError::Vsock(s))
    }
}

/// A minimal HTTP/1.1 PUT to Firecracker's API socket; expects 200/204.
fn api_put(sock: &Path, path: &str, body: &str) -> Result<(), FcError> {
    let mut s = UnixStream::connect(sock)?;
    let req = format!(
        "PUT {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes())?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf); // Connection: close → EOF (or times out)
    let resp = String::from_utf8_lossy(&buf);
    let status = resp.lines().next().unwrap_or("");
    if status.contains(" 204") || status.contains(" 200") {
        Ok(())
    } else {
        Err(FcError::Api(format!("{path} -> {status}")))
    }
}

fn wait_for_path(p: &Path, timeout: Duration) -> Result<(), FcError> {
    let start = Instant::now();
    while !p.exists() {
        if start.elapsed() > timeout {
            return Err(FcError::Timeout(format!("waiting for {}", p.display())));
        }
        sleep(Duration::from_millis(50));
    }
    Ok(())
}
