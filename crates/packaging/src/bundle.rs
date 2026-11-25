// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Finding bundled binaries at run time. The engine takes the bundle
//! directory as a plain input and never looks on the user's `PATH`: software
//! the user happens to have installed is never reused.

use std::path::{Path, PathBuf};

/// The target triple of this build, for picking a per-architecture tree.
pub fn host_triple() -> Option<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Some("aarch64-apple-darwin"),
        ("x86_64", "macos") => Some("x86_64-apple-darwin"),
        ("x86_64", "linux") => Some("x86_64-unknown-linux-gnu"),
        ("aarch64", "linux") => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryStatus {
    Executable,
    NotExecutable,
    Missing,
}

impl BinaryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            BinaryStatus::Executable => "executable",
            BinaryStatus::NotExecutable => "NOT EXECUTABLE",
            BinaryStatus::Missing => "MISSING",
        }
    }
}

/// The directory that holds every bundled service, one tree per target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    root: PathBuf,
    triple: String,
}

impl Bundle {
    pub fn new(root: impl Into<PathBuf>, triple: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            triple: triple.into(),
        }
    }

    /// Where the bundle is: an explicit directory wins. Inside an app bundle
    /// (`Contents/MacOS/<engine>`) it is `Contents/Resources/sidecars`;
    /// otherwise `sidecars` beside the engine binary.
    pub fn locate(explicit: Option<PathBuf>, engine_binary: &Path, triple: &str) -> Self {
        let beside = engine_binary.parent().unwrap_or(Path::new("."));
        let root = explicit.unwrap_or_else(|| {
            if beside.ends_with("Contents/MacOS") {
                beside.with_file_name("Resources").join("sidecars")
            } else {
                beside.join("sidecars")
            }
        });
        Self::new(root, triple)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The tree of one directory-shaped service for this architecture.
    pub fn dir(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}-{}", self.triple))
    }

    /// A program of one service: in this architecture's tree, or else in
    /// `<name>/`, where the app keeps single programs fused for both
    /// architectures. When neither exists, the per-architecture path is
    /// returned so the startup report names what is missing.
    pub fn binary(&self, name: &str, relative: &str) -> PathBuf {
        let own = self.dir(name).join(relative);
        let fused = self.root.join(name).join(relative);
        if !own.exists() && fused.exists() {
            fused
        } else {
            own
        }
    }

    /// Whether a resolved path can be run, for the startup report.
    pub fn status(path: &Path) -> BinaryStatus {
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() && is_executable(&meta) => BinaryStatus::Executable,
            Ok(_) => BinaryStatus::NotExecutable,
            Err(_) => BinaryStatus::Missing,
        }
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_explicit_or_inside_the_app_or_beside_the_engine() {
        let triple = "x86_64-apple-darwin";
        let explicit = Bundle::locate(Some("/b".into()), Path::new("/x/engine"), triple);
        assert_eq!(explicit.root(), Path::new("/b"));
        let in_app = Bundle::locate(
            None,
            Path::new("/A/Isonomic.app/Contents/MacOS/isonomic-capsule"),
            triple,
        );
        assert_eq!(
            in_app.root(),
            Path::new("/A/Isonomic.app/Contents/Resources/sidecars")
        );
        let beside = Bundle::locate(None, Path::new("/opt/isonomic/isonomic-capsule"), triple);
        assert_eq!(beside.root(), Path::new("/opt/isonomic/sidecars"));
        assert_eq!(
            beside.binary("postgres", "bin/initdb"),
            Path::new("/opt/isonomic/sidecars/postgres-x86_64-apple-darwin/bin/initdb")
        );
    }

    #[test]
    fn a_fused_program_is_found_when_there_is_no_per_architecture_tree() {
        let root = tempfile::tempdir().unwrap();
        let bundle = Bundle::new(root.path(), "aarch64-apple-darwin");
        let expected = root.path().join("cache-aarch64-apple-darwin/cache-server");
        assert_eq!(bundle.binary("cache", "cache-server"), expected);
        std::fs::create_dir_all(root.path().join("cache")).unwrap();
        std::fs::write(root.path().join("cache/cache-server"), "fused").unwrap();
        let fused = root.path().join("cache/cache-server");
        assert_eq!(bundle.binary("cache", "cache-server"), fused);
        std::fs::create_dir_all(expected.parent().unwrap()).unwrap();
        std::fs::write(&expected, "own").unwrap();
        assert_eq!(bundle.binary("cache", "cache-server"), expected);
    }

    #[cfg(unix)]
    #[test]
    fn status_tells_missing_from_not_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool");
        assert_eq!(Bundle::status(&path), BinaryStatus::Missing);
        std::fs::write(&path, "x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(Bundle::status(&path), BinaryStatus::NotExecutable);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(Bundle::status(&path), BinaryStatus::Executable);
        assert!(host_triple().is_some());
    }
}
