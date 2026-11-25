// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Capsule directory layout (architecture spec, section 7). Everything the
//! engine owns sits under one root. Sockets live in a `run` directory whose
//! path is short enough for the OS limit on socket paths.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use isonomic_proto::locate;

use crate::error::CoreError;

// The rule for where a Capsule and its sockets live is shared with the
// window through the SDK, so both sides always compute the same path.
pub use isonomic_proto::locate::{default_root, ENV_CAPSULE_DIR, SOCKET_PATH_MAX};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsulePaths {
    root: PathBuf,
    run: PathBuf,
}

impl CapsulePaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let run = locate::run_dir(&root, user_id());
        Self { root, run }
    }

    /// Pick the root: an explicit override wins, else the per-user default.
    pub fn resolve(
        override_dir: Option<OsString>,
        home: Option<PathBuf>,
    ) -> Result<Self, CoreError> {
        if let Some(dir) = override_dir.map(PathBuf::from) {
            if !dir.is_absolute() {
                return Err(CoreError::RelativePath {
                    name: ENV_CAPSULE_DIR,
                    value: dir,
                });
            }
            return Ok(Self::new(dir));
        }
        let home = home.ok_or(CoreError::NoHome(ENV_CAPSULE_DIR))?;
        Ok(Self::new(default_root(&home)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The marker naming the Capsule's data format (`format::open`).
    pub fn capsule_marker(&self) -> PathBuf {
        self.root.join("capsule.json")
    }

    /// The embedded state store.
    pub fn registry_db(&self) -> PathBuf {
        self.root.join("registry.db")
    }

    /// The secret capability tokens are derived from. It never leaves the
    /// engine.
    pub fn token_key(&self) -> PathBuf {
        self.root.join("secrets").join("token.key")
    }

    /// Where downloaded local models are kept, one file per model.
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    /// Where apps' blobs are stored, one file per blob.
    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs")
    }

    /// Where sockets live. Usually `<root>/run`; a short per-user directory
    /// when the root is too deep for a socket path.
    pub fn run_dir(&self) -> &Path {
        &self.run
    }

    /// The socket the broker listens on.
    pub fn engine_socket(&self) -> PathBuf {
        self.run.join("engine.sock")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Data directory of one service.
    pub fn service_dir(&self, name: &str) -> PathBuf {
        self.root.join("services").join(name)
    }

    /// Socket directory of one service.
    pub fn service_socket_dir(&self, name: &str) -> PathBuf {
        self.run.join(name)
    }

    /// Create the layout. The run directory is made user-only, and one that
    /// already exists but belongs to someone else is refused.
    pub fn create_dirs(&self) -> Result<(), CoreError> {
        for dir in [
            self.root.clone(),
            self.logs_dir(),
            self.root.join("services"),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::create_dir_all(&self.run)?;
        secure_dir(&self.run)?;
        Ok(())
    }
}

#[cfg(unix)]
fn user_id() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn user_id() -> u32 {
    0
}

#[cfg(unix)]
fn secure_dir(dir: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::metadata(dir)?;
    if meta.uid() != user_id() {
        return Err(CoreError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} belongs to another user", dir.display()),
        )));
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_dir(_dir: &Path) -> Result<(), CoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_hangs_off_one_root() {
        let paths = CapsulePaths::new("/c");
        assert_eq!(paths.registry_db(), Path::new("/c/registry.db"));
        assert_eq!(paths.run_dir(), Path::new("/c/run"));
        assert_eq!(paths.engine_socket(), Path::new("/c/run/engine.sock"));
        assert_eq!(paths.logs_dir(), Path::new("/c/logs"));
        assert_eq!(
            paths.service_dir("postgres"),
            Path::new("/c/services/postgres")
        );
        assert_eq!(
            paths.service_socket_dir("postgres"),
            Path::new("/c/run/postgres")
        );
    }

    #[test]
    fn override_wins_over_home_and_must_be_absolute() {
        let home = Some(PathBuf::from("/home/u"));
        let overridden = CapsulePaths::resolve(Some("/data/x".into()), home.clone()).unwrap();
        assert_eq!(overridden.root(), Path::new("/data/x"));
        let defaulted = CapsulePaths::resolve(None, home.clone()).unwrap();
        assert_eq!(defaulted.root(), default_root(Path::new("/home/u")));
        assert!(matches!(
            CapsulePaths::resolve(Some("relative".into()), home),
            Err(CoreError::RelativePath { .. })
        ));
        assert!(matches!(
            CapsulePaths::resolve(None, None),
            Err(CoreError::NoHome(_))
        ));
    }

    #[test]
    fn a_deep_root_gets_a_short_run_dir_that_fits() {
        let deep = format!("/{}", "d".repeat(150));
        let paths = CapsulePaths::new(&deep);
        assert!(paths.run_dir().starts_with("/tmp"));
        assert!(
            paths
                .run_dir()
                .join(locate::LONGEST_RUN_ENTRY)
                .as_os_str()
                .len()
                <= SOCKET_PATH_MAX
        );
        // Same root, same directory; different root, different directory.
        assert_eq!(paths.run_dir(), CapsulePaths::new(&deep).run_dir());
        assert_ne!(
            paths.run_dir(),
            CapsulePaths::new(format!("{deep}x")).run_dir()
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_dirs_makes_the_run_dir_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let paths = CapsulePaths::new(tmp.path().join("capsule"));
        paths.create_dirs().unwrap();
        assert!(paths.logs_dir().is_dir());
        let mode = std::fs::metadata(paths.run_dir())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}
