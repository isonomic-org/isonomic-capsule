//! Generic service supervision: spawn a binary in its own process group with
//! an allow-listed environment, two-phase readiness probing, crash recovery on
//! the backoff schedule with the circuit breaker, registry bookkeeping, and
//! teardown on drop.
//!
//! Spec: architecture section 4.2. Spawning lives in `spawn.rs`, the crash and
//! hang monitors in `monitor.rs`, tests in `service_tests.rs`: one
//! responsibility per file, every file small.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Mutex};
use tokio::time::Instant;

use capsule_core::ServiceState;
use registry::ServiceRegistry;

use crate::endpoint::Endpoint;
use crate::env::ChildEnv;
use crate::logsink::{LogSink, DEFAULT_ROTATE_BYTES};
use crate::monitor::{run_monitor, run_steady_probe};
use crate::probe::Readiness;
use crate::spawn::{alloc_endpoint, kill_group_now, spawn_child, terminate_group};
use crate::{restart_backoff_secs, starting_probe_delay};

/// Restart window for the circuit breaker (spec: >5 restarts in 10 minutes).
pub const BREAKER_WINDOW: Duration = Duration::from_secs(600);

/// Deliberate-stop deadline per service before the process group is killed
/// (spec: a 15 second grace, then a forced stop).
pub const DEFAULT_STOP_GRACE: Duration = Duration::from_secs(15);

/// Signal a deliberate stop sends first. Most services take `SIGTERM`;
/// PostgreSQL wants `SIGINT`, which is exactly what `pg_ctl stop -m fast`
/// sends — roll back open transactions and shut down without waiting for
/// clients to disconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    Term,
    Int,
}

impl StopSignal {
    #[cfg(unix)]
    pub(crate) fn as_libc(self) -> libc::c_int {
        match self {
            StopSignal::Term => libc::SIGTERM,
            StopSignal::Int => libc::SIGINT,
        }
    }
}

/// A config file rendered from a template before every start. The body's
/// `{port}`, `{host}`, `{data_dir}`, `{socket}` and `{socket_dir}`
/// placeholders are substituted at spawn time, then written to `dest`.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    pub dest: PathBuf,
    pub template: String,
}

/// Configuration for one supervised service. Args and config-file bodies are
/// templated: `{port}`, `{host}`, `{data_dir}`, `{socket}` and `{socket_dir}`
/// are substituted at spawn time. The engine owns ports and paths; nothing
/// hardcodes one.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub name: String,
    pub binary_path: PathBuf,
    pub args: Vec<String>,
    pub data_dir: Option<PathBuf>,
    pub readiness: Readiness,
    /// `None` allocates an ephemeral port.
    pub port: Option<u16>,
    /// When set, the service listens on this Unix socket and no port is
    /// allocated.
    pub socket: Option<PathBuf>,
    /// The child's whole environment. Nothing is inherited from the engine.
    pub env: ChildEnv,
    pub start_timeout: Duration,
    /// Longest wait between readiness probes while starting. Probing ramps
    /// up to it from 50 ms.
    pub probe_interval: Duration,
    pub steady_probe_interval: Duration,
    pub probe_timeout: Duration,
    /// Backoff schedule between restarts (injectable for tests).
    pub backoff: fn(u32) -> Duration,
    /// Directory for the rotating log file; `None` keeps logs in memory only.
    pub logs_dir: Option<PathBuf>,
    pub log_rotate_bytes: u64,
    /// Config files rendered from templates and written before every spawn.
    pub config_files: Vec<ConfigFile>,
    /// Signal sent first on a deliberate stop.
    pub stop_signal: StopSignal,
    /// How long the service gets to exit on its own before it is killed.
    pub stop_grace: Duration,
}

impl ServiceConfig {
    pub fn new(name: impl Into<String>, binary_path: PathBuf) -> Self {
        Self {
            name: name.into(),
            binary_path,
            args: Vec::new(),
            data_dir: None,
            readiness: Readiness::RedisPing,
            port: None,
            socket: None,
            env: ChildEnv::minimal(),
            start_timeout: Duration::from_secs(60),
            probe_interval: Duration::from_secs(5),
            steady_probe_interval: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(2),
            backoff: default_backoff,
            logs_dir: None,
            log_rotate_bytes: DEFAULT_ROTATE_BYTES,
            config_files: Vec::new(),
            stop_signal: StopSignal::Term,
            stop_grace: DEFAULT_STOP_GRACE,
        }
    }

    /// Attach a config file rendered from `template` to `dest` at boot.
    pub fn with_config_file(mut self, dest: PathBuf, template: impl Into<String>) -> Self {
        self.config_files.push(ConfigFile {
            dest,
            template: template.into(),
        });
        self
    }
}

fn default_backoff(attempt: u32) -> Duration {
    Duration::from_secs(restart_backoff_secs(attempt))
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("{0} is degraded (circuit breaker tripped); see last_logs()")]
    Degraded(String),
    #[error("{0} exited during startup; see last_logs()")]
    StartFailed(String),
    #[error("{0} failed to become ready within {1:?}")]
    StartTimeout(String, Duration),
    #[error("failed to spawn {0}: {1}")]
    Spawn(String, std::io::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub(crate) struct Shared {
    pub(crate) config: ServiceConfig,
    pub(crate) registry: Option<Arc<ServiceRegistry>>,
    pub(crate) logs: Arc<LogSink>,
    pub(crate) state_tx: watch::Sender<ServiceState>,
    pub(crate) state_rx: watch::Receiver<ServiceState>,
    pub(crate) restarts: std::sync::Mutex<VecDeque<Instant>>,
    pub(crate) restart_total: AtomicU32,
    pub(crate) stopping: AtomicBool,
    pub(crate) generation: AtomicU64,
    pub(crate) pgid: AtomicU32,
}

impl Shared {
    pub(crate) fn set_state(&self, state: ServiceState) {
        // Registry first, watch second: anyone woken by the watch must never
        // observe a stale registry row (a test caught exactly this race).
        if let Some(registry) = &self.registry {
            let result = if state == ServiceState::Healthy {
                registry.record_healthy(&self.config.name)
            } else {
                registry.record_state(&self.config.name, state.as_str())
            };
            if let Err(error) = result {
                self.logs.append(&format!("registry write failed: {error}"));
            }
        }
        self.state_tx.send_replace(state);
    }

    pub(crate) fn note_restart(&self) {
        self.restart_total.fetch_add(1, Ordering::SeqCst);
        if let Some(registry) = &self.registry {
            if let Err(error) = registry.record_restart(&self.config.name) {
                self.logs.append(&format!("registry write failed: {error}"));
            }
        }
    }
}

pub(crate) struct ControlState {
    pub(crate) pid: Option<u32>,
    pub(crate) addr: Option<Endpoint>,
}

/// Supervises one service. Dropping the supervisor tears the process group down
/// (parent-death watchdog: nothing survives supervisor exit).
pub struct ServiceSupervisor {
    shared: Arc<Shared>,
    control: Arc<Mutex<ControlState>>,
}

impl ServiceSupervisor {
    pub fn new(config: ServiceConfig, registry: Option<Arc<ServiceRegistry>>) -> Self {
        let logs = match &config.logs_dir {
            Some(dir) => {
                let path = dir.join(format!("{}.log", config.name));
                match LogSink::with_file(path, config.log_rotate_bytes) {
                    Ok(sink) => Arc::new(sink),
                    Err(_) => Arc::new(LogSink::memory_only()),
                }
            }
            None => Arc::new(LogSink::memory_only()),
        };
        let (state_tx, state_rx) = watch::channel(ServiceState::Stopped);
        Self {
            shared: Arc::new(Shared {
                config,
                registry,
                logs,
                state_tx,
                state_rx,
                restarts: std::sync::Mutex::new(VecDeque::new()),
                restart_total: AtomicU32::new(0),
                stopping: AtomicBool::new(false),
                generation: AtomicU64::new(0),
                pgid: AtomicU32::new(0),
            }),
            control: Arc::new(Mutex::new(ControlState {
                pid: None,
                addr: None,
            })),
        }
    }

    pub fn name(&self) -> &str {
        &self.shared.config.name
    }

    pub fn state(&self) -> ServiceState {
        *self.shared.state_tx.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<ServiceState> {
        self.shared.state_rx.clone()
    }

    pub fn last_logs(&self) -> Vec<String> {
        self.shared.logs.last_lines()
    }

    pub fn restart_total(&self) -> u32 {
        self.shared.restart_total.load(Ordering::SeqCst)
    }

    /// Where the service listens, once it has been started.
    pub async fn endpoint(&self) -> Option<Endpoint> {
        self.control.lock().await.addr.clone()
    }

    pub async fn pid(&self) -> Option<u32> {
        self.control.lock().await.pid
    }

    /// Start the service (idempotent): spawn, probe to readiness, then hand the
    /// child to the monitor for crash recovery. Returns where it listens.
    pub async fn start(&self) -> Result<Endpoint, ServiceError> {
        match self.state() {
            ServiceState::Degraded => {
                return Err(ServiceError::Degraded(self.shared.config.name.clone()))
            }
            ServiceState::Healthy => {
                if let Some(addr) = self.endpoint().await {
                    return Ok(addr);
                }
            }
            _ => {}
        }

        let mut ctl = self.control.lock().await;
        match *self.shared.state_tx.borrow() {
            ServiceState::Degraded => {
                return Err(ServiceError::Degraded(self.shared.config.name.clone()))
            }
            ServiceState::Healthy => {
                if let Some(addr) = ctl.addr.clone() {
                    return Ok(addr);
                }
            }
            _ => {}
        }

        if let Some(data_dir) = &self.shared.config.data_dir {
            tokio::fs::create_dir_all(data_dir).await?;
        }

        let addr = match ctl.addr.clone() {
            Some(addr) => addr,
            None => {
                let addr = alloc_endpoint(&self.shared.config)?;
                ctl.addr = Some(addr.clone());
                if let Some(registry) = &self.shared.registry {
                    let name = &self.shared.config.name;
                    let _ = match &addr {
                        Endpoint::Tcp(tcp) => registry.record_port(name, tcp.port()),
                        Endpoint::Unix(path) => registry.record_socket(name, path),
                    };
                }
                addr
            }
        };

        self.shared.stopping.store(false, Ordering::SeqCst);
        let generation = self.shared.generation.load(Ordering::SeqCst);
        self.shared.set_state(ServiceState::Starting);

        let (mut child, pid) = spawn_child(&self.shared, &addr)
            .map_err(|e| ServiceError::Spawn(self.shared.config.name.clone(), e))?;
        ctl.pid = Some(pid);
        self.shared.pgid.store(pid, Ordering::SeqCst);

        let deadline = Instant::now() + self.shared.config.start_timeout;
        let mut attempt = 0;
        loop {
            if self
                .shared
                .config
                .readiness
                .check(&addr, self.shared.config.probe_timeout)
                .await
            {
                break;
            }
            if child.try_wait().ok().flatten().is_some() {
                ctl.pid = None;
                self.shared.pgid.store(0, Ordering::SeqCst);
                self.shared.set_state(ServiceState::Stopped);
                return Err(ServiceError::StartFailed(self.shared.config.name.clone()));
            }
            if Instant::now() >= deadline {
                let _ = child.start_kill();
                let _ = child.wait().await;
                ctl.pid = None;
                self.shared.pgid.store(0, Ordering::SeqCst);
                self.shared.set_state(ServiceState::Stopped);
                return Err(ServiceError::StartTimeout(
                    self.shared.config.name.clone(),
                    self.shared.config.start_timeout,
                ));
            }
            tokio::time::sleep(starting_probe_delay(
                attempt,
                self.shared.config.probe_interval,
            ))
            .await;
            attempt += 1;
        }

        self.shared.set_state(ServiceState::Healthy);
        tokio::spawn(run_monitor(
            Arc::clone(&self.shared),
            Arc::clone(&self.control),
            child,
            generation,
        ));
        tokio::spawn(run_steady_probe(
            Arc::clone(&self.shared),
            Arc::clone(&self.control),
            addr.clone(),
            generation,
        ));
        Ok(addr)
    }

    /// Deliberate stop: the configured stop signal to the process group, the
    /// configured grace period, then SIGKILL.
    pub async fn stop(&self) {
        stop_with(&self.shared, &self.control).await;
    }

    /// "Report + retry": clear the breaker window and return to Stopped.
    pub async fn reset(&self) {
        stop_with(&self.shared, &self.control).await;
        self.shared.restarts.lock().expect("restarts lock").clear();
    }
}

impl Drop for ServiceSupervisor {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        let pgid = self.shared.pgid.swap(0, Ordering::SeqCst);
        kill_group_now(pgid);
    }
}

async fn stop_with(shared: &Arc<Shared>, control: &Arc<Mutex<ControlState>>) {
    shared.stopping.store(true, Ordering::SeqCst);
    shared.generation.fetch_add(1, Ordering::SeqCst);
    let pid = {
        let mut ctl = control.lock().await;
        ctl.pid.take()
    };
    if let Some(pid) = pid {
        terminate_group(pid, shared.config.stop_signal, shared.config.stop_grace).await;
    }
    shared.pgid.store(0, Ordering::SeqCst);
    shared.set_state(ServiceState::Stopped);
}

#[cfg(all(test, unix))]
#[path = "service_tests.rs"]
mod tests;
