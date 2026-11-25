// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! PostgreSQL as a supervised service (architecture spec, sections 4.2, 4.5).
//!
//! - First-time setup: `initdb` runs once, into a staging directory that is
//!   renamed into place only on success, so a half-built cluster is never
//!   mistaken for a real one.
//! - Exposure: a Unix socket in a user-only directory inside the Capsule.
//!   There is no network listener at all.
//! - Unclean shutdown: detected from the leftover lock file; the start is
//!   then given recovery time before it counts as failed.
//! - Stop: the fast-shutdown signal (roll back, do not wait for clients).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use capsule_core::Profile;

use crate::env::ChildEnv;
use crate::probe::Readiness;
use crate::service::{ServiceConfig, StopSignal};

/// Service name in the registry and on the wire.
pub const NAME: &str = "postgres";
/// The cluster's bootstrap role. Apps get their own roles later.
pub const SUPERUSER: &str = "isonomic";
/// Database the readiness probe connects to.
pub const DATABASE: &str = "postgres";
/// Names the socket file (`.s.PGSQL.<port>`). No TCP port is ever opened.
pub const SOCKET_PORT: u16 = 5432;
/// Start deadline after a clean shutdown.
pub const START_TIMEOUT: Duration = Duration::from_secs(90);
/// Start deadline when the last shutdown was unclean and recovery must run.
pub const RECOVERY_START_TIMEOUT: Duration = Duration::from_secs(300);

/// Binaries and directories, all supplied by the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresLayout {
    pub postgres: PathBuf,
    pub initdb: PathBuf,
    pub data_dir: PathBuf,
    pub socket_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PostgresError {
    #[error("first-time database setup failed ({status}): {detail}")]
    InitFailed { status: String, detail: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// The socket file PostgreSQL creates inside `socket_dir`.
pub fn socket_path(socket_dir: &Path) -> PathBuf {
    socket_dir.join(format!(".s.PGSQL.{SOCKET_PORT}"))
}

/// A cluster exists once `initdb` has finished and been renamed into place.
pub fn is_initialized(data_dir: &Path) -> bool {
    data_dir.join("PG_VERSION").is_file()
}

/// PostgreSQL removes its lock file on a clean stop, so one left behind means
/// the last run was killed or the machine went down.
pub fn had_unclean_shutdown(data_dir: &Path) -> bool {
    data_dir.join("postmaster.pid").exists()
}

fn staging_dir(data_dir: &Path) -> PathBuf {
    let mut name = data_dir.file_name().unwrap_or_default().to_os_string();
    name.push(".init");
    data_dir.with_file_name(name)
}

/// Run first-time setup if the cluster does not exist. Returns `true` when it
/// was created by this call.
pub async fn ensure_initialized(
    layout: &PostgresLayout,
    env: &ChildEnv,
) -> Result<bool, PostgresError> {
    if is_initialized(&layout.data_dir) {
        return Ok(false);
    }
    let staging = staging_dir(&layout.data_dir);
    if staging.exists() {
        tokio::fs::remove_dir_all(&staging).await?; // a setup that was interrupted
    }
    if let Some(parent) = layout.data_dir.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let output = tokio::process::Command::new(&layout.initdb)
        .arg("-D")
        .arg(&staging)
        .args(["-U", SUPERUSER])
        .args(["--auth-local=trust", "--auth-host=reject"])
        .args(["--encoding=UTF8", "--locale=C"])
        .env_clear()
        .envs(env.vars())
        .stdin(Stdio::null())
        .output()
        .await?;
    if !output.status.success() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail: Vec<&str> = stderr.lines().rev().take(5).collect();
        return Err(PostgresError::InitFailed {
            status: output.status.to_string(),
            detail: detail.into_iter().rev().collect::<Vec<_>>().join(" | "),
        });
    }
    if layout.data_dir.exists() {
        // Only an empty leftover directory is in the way; this fails otherwise.
        tokio::fs::remove_dir(&layout.data_dir).await?;
    }
    tokio::fs::rename(&staging, &layout.data_dir).await?;
    Ok(true)
}

/// The supervised-service definition. `unclean` extends the start deadline;
/// the profile sets the shared buffers.
pub fn service(
    layout: &PostgresLayout,
    profile: Profile,
    env: ChildEnv,
    unclean: bool,
) -> ServiceConfig {
    let mut config = ServiceConfig::new(NAME, layout.postgres.clone());
    let port = SOCKET_PORT.to_string();
    let buffers = format!("shared_buffers={}", profile.shared_buffers());
    config.args = [
        "-D",
        "{data_dir}",
        "-k",
        "{socket_dir}",
        "-p",
        port.as_str(),
        "-c",
        "listen_addresses=",
        "-c",
        "unix_socket_permissions=0700",
        "-c",
        buffers.as_str(),
    ]
    .map(String::from)
    .to_vec();
    config.data_dir = Some(layout.data_dir.clone());
    // The server's own record of itself, should the engine's be lost.
    config.own_pid_file = Some(layout.data_dir.join("postmaster.pid"));
    config.socket = Some(socket_path(&layout.socket_dir));
    config.env = env;
    config.readiness = Readiness::PostgresQuery {
        user: SUPERUSER.to_string(),
        database: DATABASE.to_string(),
    };
    config.start_timeout = if unclean {
        RECOVERY_START_TIMEOUT
    } else {
        START_TIMEOUT
    };
    config.stop_signal = StopSignal::Int;
    config
}

/// What `prepare` found and did, for the startup log and the report.
#[derive(Debug)]
pub struct Prepared {
    pub config: ServiceConfig,
    /// The cluster was created by this call.
    pub initialized_now: bool,
    /// The previous run did not shut down cleanly.
    pub unclean_shutdown: bool,
}

/// Everything that must happen before the supervisor starts PostgreSQL: a
/// user-only socket directory, first-time setup, unclean-shutdown detection.
pub async fn prepare(
    layout: &PostgresLayout,
    profile: Profile,
    env: ChildEnv,
) -> Result<Prepared, PostgresError> {
    tokio::fs::create_dir_all(&layout.socket_dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let user_only = std::fs::Permissions::from_mode(0o700);
        tokio::fs::set_permissions(&layout.socket_dir, user_only).await?;
    }
    let initialized_now = ensure_initialized(layout, &env).await?;
    let unclean_shutdown = !initialized_now && had_unclean_shutdown(&layout.data_dir);
    Ok(Prepared {
        config: service(layout, profile, env, unclean_shutdown),
        initialized_now,
        unclean_shutdown,
    })
}

#[cfg(all(test, unix))]
#[path = "postgres_tests.rs"]
mod tests;
