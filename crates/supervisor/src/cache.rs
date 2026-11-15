//! The cache and queue service as a supervised service (architecture spec,
//! sections 4.2, 4.5).
//!
//! - Exposure: a Unix socket in a user-only directory inside the Capsule; no
//!   TCP port (`port 0`).
//! - Memory: capped by the hardware profile. On Low, eviction is expected, so
//!   the least recently used keys go first; elsewhere a full cache refuses
//!   writes rather than silently dropping queued work.
//! - Persistence: off until a use needs it (snapshots and the append log are
//!   both disabled).
//! - Readiness: `PING` over the socket. Stop: the default termination signal,
//!   which exits at once with nothing to save.

use std::path::{Path, PathBuf};
use std::time::Duration;

use capsule_core::Profile;

use crate::env::ChildEnv;
use crate::probe::Readiness;
use crate::service::{ServiceConfig, StopSignal};

/// Service name in the registry and on the wire.
pub const NAME: &str = "cache";
/// The pinned sidecar that provides it.
pub const SIDECAR: &str = "valkey";
/// The program inside the sidecar's install directory.
pub const PROGRAM: &str = "valkey-server";
pub const START_TIMEOUT: Duration = Duration::from_secs(30);

/// Program and directories, all supplied by the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLayout {
    pub program: PathBuf,
    pub data_dir: PathBuf,
    pub socket_dir: PathBuf,
}

impl CacheLayout {
    pub fn socket(&self) -> PathBuf {
        self.socket_dir.join("cache.sock")
    }

    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join("cache.conf")
    }
}

/// The config file, rendered before every start (`{socket}` and `{data_dir}`
/// are filled in by the supervisor). Both paths are quoted: the default
/// Capsule directory on a Mac is under "Application Support", and the cache
/// reads an unquoted space as a second argument.
pub fn config_template(profile: Profile) -> String {
    let policy = match profile {
        Profile::Low => "allkeys-lru",
        Profile::Standard | Profile::High => "noeviction",
    };
    format!(
        "# Written by the engine before every start. Edits are overwritten.\n\
         port 0\n\
         unixsocket \"{{socket}}\"\n\
         unixsocketperm 700\n\
         dir \"{{data_dir}}\"\n\
         daemonize no\n\
         protected-mode yes\n\
         save \"\"\n\
         appendonly no\n\
         maxmemory {}\n\
         maxmemory-policy {policy}\n\
         loglevel notice\n\
         logfile \"\"\n",
        profile.cache_maxmemory()
    )
}

/// The supervised-service definition.
pub fn service(layout: &CacheLayout, profile: Profile, env: ChildEnv) -> ServiceConfig {
    let mut config = ServiceConfig::new(NAME, layout.program.clone())
        .with_config_file(layout.config_path(), config_template(profile));
    config.args = vec![layout.config_path().display().to_string()];
    config.data_dir = Some(layout.data_dir.clone());
    config.socket = Some(layout.socket());
    config.env = env;
    config.readiness = Readiness::RedisPing;
    config.start_timeout = START_TIMEOUT;
    config.stop_signal = StopSignal::Term;
    config
}

/// Directories the service needs before its first start: data, and a
/// user-only socket directory.
pub async fn prepare(layout: &CacheLayout) -> std::io::Result<()> {
    tokio::fs::create_dir_all(&layout.data_dir).await?;
    user_only_dir(&layout.socket_dir).await
}

pub(crate) async fn user_only_dir(dir: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> CacheLayout {
        CacheLayout {
            program: PathBuf::from("/bundle/cache"),
            data_dir: PathBuf::from("/capsule/services/cache"),
            socket_dir: PathBuf::from("/capsule/run/cache"),
        }
    }

    #[test]
    fn listens_only_on_its_socket_with_no_persistence() {
        let text = config_template(Profile::Standard);
        for line in [
            "port 0",
            "unixsocket \"{socket}\"",
            "unixsocketperm 700",
            "dir \"{data_dir}\"",
            "save \"\"",
            "appendonly no",
        ] {
            assert!(
                text.lines().any(|l| l == line),
                "missing {line:?} in\n{text}"
            );
        }
        assert!(!text.contains("bind "), "no network listener at all");
    }

    #[test]
    fn a_capsule_under_application_support_stays_one_argument() {
        let socket = Path::new("/Users/me/Library/Application Support/Isonomic/run/cache.sock");
        let text = crate::spawn::substitute(
            &config_template(Profile::Standard),
            &crate::Endpoint::Unix(socket.to_path_buf()),
            "/Users/me/Library/Application Support/Isonomic/services/cache",
        );
        assert!(text.contains(
            "unixsocket \"/Users/me/Library/Application Support/Isonomic/run/cache.sock\"\n"
        ));
        assert!(text
            .contains("dir \"/Users/me/Library/Application Support/Isonomic/services/cache\"\n"));
    }

    #[test]
    fn memory_and_eviction_follow_the_profile() {
        let low = config_template(Profile::Low);
        assert!(low.contains("maxmemory 256mb\n") && low.contains("allkeys-lru"));
        let standard = config_template(Profile::Standard);
        assert!(standard.contains("maxmemory 1gb\n") && standard.contains("noeviction"));
        let high = config_template(Profile::High);
        assert!(high.contains("maxmemory 2gb\n") && high.contains("noeviction"));
    }

    #[test]
    fn the_service_runs_the_rendered_config_on_its_socket() {
        let config = service(&layout(), Profile::Low, ChildEnv::minimal());
        assert_eq!(config.name, NAME);
        assert_eq!(config.args, ["/capsule/services/cache/cache.conf"]);
        assert_eq!(
            config.socket.as_deref(),
            Some(Path::new("/capsule/run/cache/cache.sock"))
        );
        assert_eq!(config.config_files.len(), 1);
        assert_eq!(config.config_files[0].dest, layout().config_path());
        assert!(matches!(config.readiness, Readiness::RedisPing));
        assert!(matches!(config.stop_signal, StopSignal::Term));
    }
}
