//! Broker: the engine's socket server (architecture spec, section 4.6).
//!
//! It serves the `system` interface (status of every service, a
//! subscription to status changes, recent logs, the hardware profile and an
//! ordered shutdown), the `apps` interface, which grants capability
//! tokens, and the app interfaces: `blobs` and `events` so far. Every request is checked against its caller's token first.

pub mod apps;
pub mod blobs;
mod conn;
pub mod events;
pub mod host;
mod random;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use isonomic_proto::{HardwareProfile, ServiceStatus, SystemStatus};
use tokio::net::UnixListener;
use tokio::sync::broadcast;

pub use apps::{Apps, TokenKey};
pub use blobs::Blobs;
pub use events::Events;
pub use host::StackSystem;

/// Protocol versions this engine speaks.
pub const ENGINE_VERSIONS: &[u32] = &[isonomic_proto::PROTOCOL_VERSION];

/// What the broker needs from the engine. The real implementation wraps the
/// supervisor's stack; tests use a fake.
pub trait System: Send + Sync + 'static {
    fn status(&self) -> SystemStatus;
    /// A receiver that sees every status change from now on.
    fn changes(&self) -> broadcast::Receiver<ServiceStatus>;
    /// Recent log lines of one service; `None` if there is no such service.
    fn logs(&self, service: &str, limit: usize) -> Option<Vec<String>>;
    fn profile(&self) -> HardwareProfile;
    /// Ask the engine to stop everything in order.
    fn request_shutdown(&self);
}

#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("another engine is already serving {0}")]
    AlreadyRunning(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// The bound engine socket together with the lock that makes this engine the
/// only one serving its Capsule. Dropping it releases both.
pub struct EngineSocket {
    listener: UnixListener,
    _lock: std::fs::File,
}

/// Bind the engine socket, user-only. Single instance is decided by an
/// advisory lock on `engine.lock` beside the socket, not by probing the
/// socket: the kernel drops the lock when its holder dies, so a crashed
/// engine never blocks the next one, and a live one always does. Whatever
/// socket file is left behind once the lock is ours is stale and replaced.
/// Must be called inside a Tokio runtime.
pub fn bind(socket: &Path) -> Result<EngineSocket, BindError> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(socket.with_extension("lock"))?;
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the descriptor is open and owned by `lock` for this call.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            return Err(match error.kind() {
                std::io::ErrorKind::WouldBlock => BindError::AlreadyRunning(socket.to_path_buf()),
                _ => BindError::Io(error),
            });
        }
    }
    match std::fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(socket)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(EngineSocket {
        listener,
        _lock: lock,
    })
}

/// What the broker serves beyond `system`: the approved apps (which check
/// every token) and each app interface.
pub struct Services {
    pub apps: Apps,
    pub blobs: Blobs,
    pub events: Events,
}

/// Accept connections until the task is dropped.
pub async fn serve(socket: EngineSocket, system: Arc<dyn System>, services: Arc<Services>) {
    loop {
        match socket.listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(conn::handle(
                    stream,
                    Arc::clone(&system),
                    Arc::clone(&services),
                ));
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
}
