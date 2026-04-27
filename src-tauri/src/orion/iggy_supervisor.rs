//! Iggy server sidecar lifecycle.
//!
//! Spawns the `iggy-server` binary as a child process, waits for its TCP
//! port to accept connections, and supervises it through the lifetime of
//! the OrionCore. On crash, it restarts up to 3 times in 60 seconds; if it
//! crashes more than that, the supervisor gives up and emits a
//! `Topic::GovernanceInbound` envelope so the UI can surface "broker is
//! unstable."
//!
//! The binary is expected at one of these locations, in order:
//!
//! 1. Path supplied via the `ORIONII_IGGY_SERVER` env var (dev override).
//! 2. Tauri's `externalBin` resolved sidecar path
//!    (`iggy-server-{target-triple}` next to the OrionII executable in a
//!    bundled build).
//! 3. `iggy-server` on `PATH` (developer fallback).
//!
//! The data directory is `{config_dir}/OrionII/iggy/`. It survives
//! restart and is isolated per OS user. iggy-server is launched with
//! `--system.path={data_dir}` and the configured TCP port.
//!
//! Phase 2b ships the supervisor logic compile-clean; the sidecar binary
//! itself is **not vendored** in this repo — see ADR-002 § "Binary
//! vendoring" for the manual install instructions until the Phase 2.1
//! `build.rs` checksum-verified download lands.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tauri::async_runtime::JoinHandle;
use thiserror::Error;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::orion::bus::{Envelope, SharedBus, Topic};

/// How long to wait for the sidecar to start accepting TCP connections
/// before giving up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

/// Crash supervision: 3 restarts in 60s before declaring instability.
const MAX_RESTARTS_PER_WINDOW: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(
        "could not locate iggy-server binary; set ORIONII_IGGY_SERVER or place the binary on PATH"
    )]
    BinaryMissing,
    #[error("could not determine data directory")]
    NoDataDir,
    #[error("spawn iggy-server: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("iggy-server did not begin accepting TCP connections within {0:?}")]
    StartupTimeout(Duration),
}

pub struct IggySupervisor {
    endpoint: String,
    /// Held to abort the supervisor watcher when the OrionCore drops.
    /// Aborting the watcher releases its strong handle to the child,
    /// which causes Drop to send SIGKILL via tokio::process::Child::kill.
    _watcher: JoinHandle<()>,
    /// Mutex over the live child so the watcher and the explicit-kill
    /// path don't race on `kill()`.
    child: Arc<Mutex<Option<Child>>>,
}

impl IggySupervisor {
    /// Spawn and supervise an iggy-server child. Returns once the child
    /// is accepting TCP connections on `port`.
    pub async fn start(
        port: u16,
        bus: SharedBus,
        supervisor_soul_ref: String,
    ) -> Result<Self, SupervisorError> {
        let binary = locate_binary()?;
        let data_dir = data_directory()?;
        std::fs::create_dir_all(&data_dir).map_err(SupervisorError::Spawn)?;

        let endpoint = format!("tcp://127.0.0.1:{port}");
        let child = spawn_child(&binary, &data_dir, port).await?;
        wait_for_port(port, STARTUP_TIMEOUT).await?;

        let child = Arc::new(Mutex::new(Some(child)));
        let watcher = supervise(
            child.clone(),
            binary.clone(),
            data_dir.clone(),
            port,
            bus.clone(),
            supervisor_soul_ref,
        );

        Ok(Self {
            endpoint,
            _watcher: watcher,
            child,
        })
    }

    /// `tcp://host:port` form, suitable for handing to the iggy client builder.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for IggySupervisor {
    fn drop(&mut self) {
        // Best-effort kill. `kill` on tokio::process::Child sends SIGKILL
        // synchronously; the child reaper task reaps the zombie. We can't
        // .await here, so this is fire-and-forget.
        if let Ok(mut guard) = self.child.try_lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
    }
}

/// Resolve the iggy-server binary. See module docs for the lookup order.
fn locate_binary() -> Result<PathBuf, SupervisorError> {
    if let Ok(env_path) = std::env::var("ORIONII_IGGY_SERVER") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Ok(p);
        }
    }

    // Tauri externalBin: the bundle places `iggy-server-{triple}` next
    // to the main app binary. We don't know the exact triple at runtime
    // here, so we look for any file matching `iggy-server*` next to the
    // current executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for ext in [".exe", ""] {
                let candidate = dir.join(format!("iggy-server{ext}"));
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
            // Try with target-triple suffix variants.
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("iggy-server") {
                        return Ok(entry.path());
                    }
                }
            }
        }
    }

    // PATH fallback for developer setups.
    if let Ok(path) = std::env::var("PATH") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        for dir in path.split(separator) {
            for ext in if cfg!(windows) {
                &[".exe", ""][..]
            } else {
                &[""][..]
            } {
                let candidate = PathBuf::from(dir).join(format!("iggy-server{ext}"));
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }
    }

    Err(SupervisorError::BinaryMissing)
}

fn data_directory() -> Result<PathBuf, SupervisorError> {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let mut p = PathBuf::from(appdata);
        p.push("OrionII");
        p.push("iggy");
        return Ok(p);
    }
    if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config");
        p.push("OrionII");
        p.push("iggy");
        return Ok(p);
    }
    Err(SupervisorError::NoDataDir)
}

async fn spawn_child(
    binary: &PathBuf,
    data_dir: &PathBuf,
    port: u16,
) -> Result<Child, SupervisorError> {
    let mut cmd = Command::new(binary);
    cmd.env("IGGY_SYSTEM_PATH", data_dir);
    cmd.env("IGGY_TCP_ENABLED", "true");
    cmd.env("IGGY_TCP_ADDRESS", format!("127.0.0.1:{port}"));
    cmd.env("IGGY_HTTP_ENABLED", "false");
    cmd.env("IGGY_QUIC_ENABLED", "false");
    cmd.env("IGGY_ROOT_USERNAME", "iggy");
    cmd.env("IGGY_ROOT_PASSWORD", "iggy");
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    cmd.kill_on_drop(true);
    Ok(cmd.spawn()?)
}

async fn wait_for_port(port: u16, timeout: Duration) -> Result<(), SupervisorError> {
    let deadline = Instant::now() + timeout;
    loop {
        match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Ok(_) => return Ok(()),
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(_) => return Err(SupervisorError::StartupTimeout(timeout)),
        }
    }
}

/// Watch the child. On unexpected exit, restart up to MAX_RESTARTS_PER_WINDOW
/// times in RESTART_WINDOW. After that, give up and publish a
/// `Topic::GovernanceInbound` envelope so the UI can surface
/// "broker is unstable."
fn supervise(
    child: Arc<Mutex<Option<Child>>>,
    binary: PathBuf,
    data_dir: PathBuf,
    port: u16,
    bus: SharedBus,
    supervisor_soul_ref: String,
) -> JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let mut restart_history: Vec<Instant> = Vec::new();

        loop {
            // Wait for the child to exit. Hold the lock only briefly to
            // borrow the child for `wait`.
            let exit_status = {
                let mut guard = child.lock().await;
                let Some(handle) = guard.as_mut() else {
                    return;
                };
                handle.wait().await
            };

            // Drop the dead child handle.
            {
                let mut guard = child.lock().await;
                *guard = None;
            }

            if let Ok(status) = exit_status {
                eprintln!("[iggy-supervisor] iggy-server exited: {status}");
            }

            // Track the restart attempt within the rolling window.
            let now = Instant::now();
            restart_history.retain(|t| now.duration_since(*t) <= RESTART_WINDOW);
            restart_history.push(now);

            if restart_history.len() > MAX_RESTARTS_PER_WINDOW {
                eprintln!(
                    "[iggy-supervisor] iggy-server crashed {} times in {:?}, giving up",
                    restart_history.len(),
                    RESTART_WINDOW
                );
                publish_broker_unstable(&bus, &supervisor_soul_ref).await;
                return;
            }

            // Wait briefly before retry (let the OS clean up the socket).
            tokio::time::sleep(Duration::from_millis(500)).await;

            match spawn_child(&binary, &data_dir, port).await {
                Ok(new_child) => {
                    if let Err(error) = wait_for_port(port, STARTUP_TIMEOUT).await {
                        eprintln!(
                            "[iggy-supervisor] restarted child failed wait_for_port: {error}"
                        );
                    } else {
                        eprintln!("[iggy-supervisor] iggy-server restarted on port {port}");
                    }
                    let mut guard = child.lock().await;
                    *guard = Some(new_child);
                }
                Err(error) => {
                    eprintln!("[iggy-supervisor] failed to respawn iggy-server: {error}");
                    publish_broker_unstable(&bus, &supervisor_soul_ref).await;
                    return;
                }
            }
        }
    })
}

async fn publish_broker_unstable(bus: &SharedBus, soul_ref: &str) {
    let env = Envelope::new(
        Topic::GovernanceInbound,
        "iggy-supervisor",
        soul_ref,
        None,
        serde_json::json!({
            "kind": "broker-unstable",
            "summary": "iggy-server crashed too many times; supervisor gave up",
        }),
    );
    if let Err(error) = bus.publish(env).await {
        eprintln!(
            "[iggy-supervisor] failed to publish broker-unstable governance envelope: {error}"
        );
    }
}
