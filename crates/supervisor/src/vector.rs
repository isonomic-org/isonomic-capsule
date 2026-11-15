//! The vector database as a supervised service on the lazy tier (architecture
//! spec, sections 4.2, 4.5).
//!
//! - Exposure: 127.0.0.1 on a port the engine picks at each start, guarded by
//!   a key generated once and kept in the Capsule (never logged). It has no
//!   Unix-socket listener, so loopback with a key is the spec's fallback.
//! - Its second (RPC) listener is off; only HTTP is served.
//! - Telemetry is off: nothing leaves the machine.
//! - On the Low profile, payloads stay on disk rather than in memory.
//! - Readiness: HTTP `GET /readyz`. Stop: the termination signal.
//!
//! Pinned and tested, but its sidecar stays deferred until its first user
//! (Chat projects or the memory index, wave 3; Omar, 15 Nov).

use std::path::{Path, PathBuf};
use std::time::Duration;

use capsule_core::Profile;

use crate::env::ChildEnv;
use crate::probe::Readiness;
use crate::service::{ServiceConfig, StopSignal};

pub const NAME: &str = "vector";
pub const SIDECAR: &str = "qdrant";
pub const PROGRAM: &str = "qdrant";
pub const START_TIMEOUT: Duration = Duration::from_secs(60);
const KEY_FILE: &str = "api.key";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorLayout {
    pub program: PathBuf,
    pub data_dir: PathBuf,
}

impl VectorLayout {
    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join("vector.yaml")
    }

    pub fn key_path(&self) -> PathBuf {
        self.data_dir.join(KEY_FILE)
    }
}

/// The config file. `{host}`, `{port}` and `{data_dir}` are filled in by the
/// supervisor at each start; the key is the stored one.
pub fn config_template(profile: Profile, key: &str) -> String {
    let on_disk = matches!(profile, Profile::Low);
    format!(
        "# Written by the engine before every start. Edits are overwritten.\n\
         telemetry_disabled: true\n\
         log_level: INFO\n\
         service:\n\
         \x20 host: {{host}}\n\
         \x20 http_port: {{port}}\n\
         \x20 grpc_port: null\n\
         \x20 api_key: {key}\n\
         \x20 enable_cors: false\n\
         storage:\n\
         \x20 storage_path: \"{{data_dir}}/storage\"\n\
         \x20 snapshots_path: \"{{data_dir}}/snapshots\"\n\
         \x20 on_disk_payload: {on_disk}\n"
    )
}

/// The supervised-service definition.
pub fn service(layout: &VectorLayout, profile: Profile, key: &str, env: ChildEnv) -> ServiceConfig {
    let mut config = ServiceConfig::new(NAME, layout.program.clone())
        .with_config_file(layout.config_path(), config_template(profile, key));
    config.args = vec![
        "--config-path".to_string(),
        layout.config_path().display().to_string(),
    ];
    config.data_dir = Some(layout.data_dir.clone());
    config.env = env;
    config.readiness = Readiness::Http {
        path: "/readyz".to_string(),
    };
    config.start_timeout = START_TIMEOUT;
    config.stop_signal = StopSignal::Term;
    config
}

/// The data directory and the key: generated on first use, kept for later
/// starts, readable by the user only.
pub async fn prepare(layout: &VectorLayout) -> std::io::Result<String> {
    tokio::fs::create_dir_all(&layout.data_dir).await?;
    crate::cache::user_only_dir(&layout.data_dir).await?;
    load_or_create_key(&layout.key_path()).await
}

async fn load_or_create_key(path: &Path) -> std::io::Result<String> {
    if let Ok(existing) = tokio::fs::read_to_string(path).await {
        let key = existing.trim().to_string();
        if key.len() >= 32 && key.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(key);
        }
    }
    // The system's random source; the engine runs on macOS and Linux only.
    let mut bytes = [0u8; 32];
    {
        use tokio::io::AsyncReadExt;
        let mut random = tokio::fs::File::open("/dev/urandom").await?;
        random.read_exact(&mut bytes).await?;
    }
    let key: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    tokio::fs::write(path, &key).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_http_only_with_a_key_and_no_telemetry() {
        let text = config_template(Profile::Standard, "k3y");
        for line in [
            "telemetry_disabled: true",
            "  host: {host}",
            "  http_port: {port}",
            "  grpc_port: null",
            "  api_key: k3y",
            "  storage_path: \"{data_dir}/storage\"",
        ] {
            assert!(
                text.lines().any(|l| l == line),
                "missing {line:?} in\n{text}"
            );
        }
    }

    #[test]
    fn payloads_stay_on_disk_on_low() {
        assert!(config_template(Profile::Low, "k").contains("on_disk_payload: true"));
        assert!(config_template(Profile::High, "k").contains("on_disk_payload: false"));
    }

    #[test]
    fn the_service_has_no_fixed_port_and_probes_readyz() {
        let layout = VectorLayout {
            program: PathBuf::from("/bundle/vector"),
            data_dir: PathBuf::from("/capsule/services/vector"),
        };
        let config = service(&layout, Profile::Low, "k", ChildEnv::minimal());
        assert_eq!(config.port, None, "the engine picks the port");
        assert_eq!(config.socket, None);
        assert_eq!(config.args[1], "/capsule/services/vector/vector.yaml");
        assert!(matches!(&config.readiness, Readiness::Http { path } if path == "/readyz"));
    }

    #[tokio::test]
    async fn the_key_is_made_once_kept_and_user_only() {
        let dir = tempfile::tempdir().unwrap();
        let layout = VectorLayout {
            program: PathBuf::from("/x"),
            data_dir: dir.path().join("vector"),
        };
        let first = prepare(&layout).await.unwrap();
        let again = prepare(&layout).await.unwrap();
        assert_eq!(first, again);
        assert_eq!(first.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(layout.key_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
