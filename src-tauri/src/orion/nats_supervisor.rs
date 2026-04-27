//! NATS server sidecar lifecycle.
//!
//! Spawns a local `nats-server` process with JetStream enabled, binds it
//! to loopback only, and stores JetStream data under
//! `{config_dir}/OrionII/nats/`. This is OrionII's durable product bus
//! path; SAO still only talks to OrionII over HTTP.

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

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESTARTS_PER_WINDOW: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("could not locate nats-server binary; set ORIONII_NATS_SERVER or place it on PATH")]
    BinaryMissing,
    #[error("could not determine data directory")]
    NoDataDir,
    #[error("spawn nats-server: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("nats-server did not begin accepting TCP connections within {0:?}")]
    StartupTimeout(Duration),
}

pub struct NatsSupervisor {
    endpoint: String,
    _watcher: JoinHandle<()>,
    child: Arc<Mutex<Option<Child>>>,
}

impl NatsSupervisor {
    pub async fn start(
        port: u16,
        bus: SharedBus,
        supervisor_soul_ref: String,
    ) -> Result<Self, SupervisorError> {
        let binary = locate_binary()?;
        let data_dir = data_directory()?;
        std::fs::create_dir_all(&data_dir).map_err(SupervisorError::Spawn)?;

        let endpoint = format!("nats://127.0.0.1:{port}");
        let child = spawn_child(&binary, &data_dir, port).await?;
        wait_for_port(port, STARTUP_TIMEOUT).await?;

        let child = Arc::new(Mutex::new(Some(child)));
        let watcher = supervise(
            child.clone(),
            binary,
            data_dir,
            port,
            bus,
            supervisor_soul_ref,
        );

        Ok(Self {
            endpoint,
            _watcher: watcher,
            child,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for NatsSupervisor {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.try_lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
    }
}

fn locate_binary() -> Result<PathBuf, SupervisorError> {
    if let Ok(env_path) = std::env::var("ORIONII_NATS_SERVER") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Ok(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for ext in [".exe", ""] {
                let candidate = dir.join(format!("nats-server{ext}"));
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("nats-server") {
                        return Ok(entry.path());
                    }
                }
            }
        }
    }

    if let Ok(path) = std::env::var("PATH") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        for dir in path.split(separator) {
            for ext in if cfg!(windows) {
                &[".exe", ""][..]
            } else {
                &[""][..]
            } {
                let candidate = PathBuf::from(dir).join(format!("nats-server{ext}"));
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
        p.push("nats");
        return Ok(p);
    }
    if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config");
        p.push("OrionII");
        p.push("nats");
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
    cmd.arg("-js");
    cmd.arg("-sd").arg(data_dir);
    cmd.arg("-a").arg("127.0.0.1");
    cmd.arg("-p").arg(port.to_string());
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
            let exit_status = {
                let mut guard = child.lock().await;
                let Some(handle) = guard.as_mut() else {
                    return;
                };
                handle.wait().await
            };

            {
                let mut guard = child.lock().await;
                *guard = None;
            }

            if let Ok(status) = exit_status {
                eprintln!("[nats-supervisor] nats-server exited: {status}");
            }

            let now = Instant::now();
            restart_history.retain(|t| now.duration_since(*t) <= RESTART_WINDOW);
            restart_history.push(now);

            if restart_history.len() > MAX_RESTARTS_PER_WINDOW {
                eprintln!(
                    "[nats-supervisor] nats-server crashed {} times in {:?}, giving up",
                    restart_history.len(),
                    RESTART_WINDOW
                );
                publish_broker_unstable(&bus, &supervisor_soul_ref).await;
                return;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;

            match spawn_child(&binary, &data_dir, port).await {
                Ok(new_child) => {
                    if let Err(error) = wait_for_port(port, STARTUP_TIMEOUT).await {
                        eprintln!(
                            "[nats-supervisor] restarted child failed wait_for_port: {error}"
                        );
                    } else {
                        eprintln!("[nats-supervisor] nats-server restarted on port {port}");
                    }
                    let mut guard = child.lock().await;
                    *guard = Some(new_child);
                }
                Err(error) => {
                    eprintln!("[nats-supervisor] failed to respawn nats-server: {error}");
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
        "nats-supervisor",
        soul_ref,
        None,
        serde_json::json!({
            "kind": "broker-unstable",
            "broker": "nats",
            "summary": "nats-server crashed too many times; supervisor gave up",
        }),
    );
    if let Err(error) = bus.publish(env).await {
        eprintln!(
            "[nats-supervisor] failed to publish broker-unstable governance envelope: {error}"
        );
    }
}
