//! Process plumbing for one service: allocate where it listens, render its
//! arguments and config files, spawn it in its own process group with an
//! allow-listed environment, pump its output into the log sink, and signal or
//! kill the group.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Child;
use tokio::time::Instant;

use crate::endpoint::Endpoint;
use crate::logsink::LogSink;
use crate::service::{ServiceConfig, Shared, StopSignal};

pub(crate) fn kill_group_now(pgid: u32) {
    #[cfg(unix)]
    if pgid != 0 {
        unsafe {
            libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

pub(crate) fn kill_pid(pid: u32) {
    #[cfg(unix)]
    if pid != 0 {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(unix)]
pub(crate) async fn terminate_group(pid: u32, signal: StopSignal, grace: Duration) {
    if pid == 0 {
        return;
    }
    unsafe {
        libc::kill(-(pid as libc::pid_t), signal.as_libc());
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return;
        }
    }
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub(crate) async fn terminate_group(_pid: u32, _signal: StopSignal, _grace: Duration) {}

/// A configured socket wins; otherwise a loopback port, ephemeral unless pinned.
pub(crate) fn alloc_endpoint(config: &ServiceConfig) -> std::io::Result<Endpoint> {
    match &config.socket {
        Some(socket) => {
            if let Some(dir) = socket.parent() {
                std::fs::create_dir_all(dir)?;
            }
            Ok(Endpoint::Unix(socket.clone()))
        }
        None => alloc_addr(config.port).map(Endpoint::Tcp),
    }
}

pub(crate) fn alloc_addr(port: Option<u16>) -> std::io::Result<SocketAddr> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    match port {
        Some(p) => Ok(SocketAddr::new(ip, p)),
        None => {
            let listener = std::net::TcpListener::bind((ip, 0))?;
            let addr = listener.local_addr()?;
            drop(listener);
            Ok(addr)
        }
    }
}

/// Substitute the placeholders in a template string. A placeholder that does
/// not apply to the endpoint kind (a port on a socket, a socket on a port)
/// becomes empty.
pub(crate) fn substitute(template: &str, addr: &Endpoint, data_dir: &str) -> String {
    let (port, host, socket, socket_dir) = match addr {
        Endpoint::Tcp(tcp) => (
            tcp.port().to_string(),
            tcp.ip().to_string(),
            String::new(),
            String::new(),
        ),
        Endpoint::Unix(path) => (
            String::new(),
            String::new(),
            path.display().to_string(),
            path.parent()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
        ),
    };
    template
        .replace("{port}", &port)
        .replace("{host}", &host)
        .replace("{socket_dir}", &socket_dir)
        .replace("{socket}", &socket)
        .replace("{data_dir}", data_dir)
}

pub(crate) fn data_dir_str(config: &ServiceConfig) -> String {
    config
        .data_dir
        .as_ref()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| ".".to_string())
}

pub(crate) fn resolve_args(config: &ServiceConfig, addr: &Endpoint) -> Vec<String> {
    let data_dir = data_dir_str(config);
    config
        .args
        .iter()
        .map(|arg| substitute(arg, addr, &data_dir))
        .collect()
}

/// Render every config file from its template and write it before launch.
pub(crate) fn render_config_files(config: &ServiceConfig, addr: &Endpoint) -> std::io::Result<()> {
    let data_dir = data_dir_str(config);
    for file in &config.config_files {
        if let Some(parent) = file.dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file.dest, substitute(&file.template, addr, &data_dir))?;
    }
    Ok(())
}

pub(crate) fn spawn_child(shared: &Arc<Shared>, addr: &Endpoint) -> std::io::Result<(Child, u32)> {
    let config = &shared.config;
    render_config_files(config, addr)?;
    let mut cmd = std::process::Command::new(&config.binary_path);
    // Allow-list only: clear whatever the engine inherited from the user's
    // shell (database, loader and locale variables included), then set the
    // variables this service declares.
    cmd.args(resolve_args(config, addr))
        .env_clear()
        .envs(config.env.vars())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group so teardown can signal the whole tree.
        cmd.process_group(0);
    }
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let pid = child.id().unwrap_or(0);
    if let Some(stdout) = child.stdout.take() {
        spawn_log_pump(Arc::clone(&shared.logs), stdout, "stdout");
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_pump(Arc::clone(&shared.logs), stderr, "stderr");
    }
    shared
        .logs
        .append(&format!("spawned {} (pid {pid}) on {addr}", config.name));
    Ok((child, pid))
}

pub(crate) fn spawn_log_pump<R>(logs: Arc<LogSink>, reader: R, label: &'static str)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => logs.append(&format!("[{label}] {line}")),
                Ok(None) => break,
                Err(error) => {
                    // Say so: a pump that dies silently looks like a quiet service.
                    logs.append(&format!("[{label}] log capture stopped: {error}"));
                    break;
                }
            }
        }
    });
}
