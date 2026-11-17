// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The pinned manifest (`sidecars.json`) and the rules it must satisfy.
//!
//! Three install shapes: `file` (one executable), `directory` (an executable
//! plus the files it loads), and `build` (compile a verified source archive).
//! The macOS build is universal, so a pin is complete only when both Apple
//! slices are recorded.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Assembled from the two Apple slices; never declared in a manifest.
pub const UNIVERSAL_TRIPLE: &str = "universal-apple-darwin";
pub const APPLE_SLICES: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
const ARCHIVE_KINDS: [&str; 2] = ["tar.gz", "none"];

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("sidecars.json: {0}")]
    Invalid(String),
    #[error("sidecars.json is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: u32,
    /// Where artifacts are installed, relative to the repo root.
    pub out_dir: String,
    /// The oldest macOS the product supports, `13.3`. No installed file
    /// may say it needs a newer one (`minos`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_macos: Option<String>,
    pub sidecars: Vec<Sidecar>,
    /// Notes (`$`-prefixed keys) and anything else, kept on rewrite.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sidecar {
    pub name: String,
    pub version: String,
    pub license: String,
    /// Declared but not yet pinned: validates without hashes, never fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,
    /// Pinned, but not bundled until its milestone: never fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<bool>,
    /// `file` (default) or `directory`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<String>,
    /// The executable inside a directory install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_in_dir: Option<String>,
    /// For a directory install: the only entries of `bin/` to keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_in_bin: Option<Vec<String>>,
    /// Compile from the downloaded source archive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<Value>,
    /// The licence text in the archive or source tree, by the names it may
    /// have (the first one present is kept). The bundle's notice is built
    /// from it, since the app redistributes the program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_files: Option<Vec<String>>,
    /// Keyed by target triple. Insertion order is preserved.
    pub targets: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub url: String,
    #[serde(default)]
    pub sha256: String,
    pub archive: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_in_archive: Option<String>,
    /// This target's own build, in place of the sidecar's (one target built
    /// from source while another is a prebuilt download).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<Value>,
}

impl Sidecar {
    pub fn is_draft(&self) -> bool {
        self.draft == Some(true)
    }

    pub fn is_deferred(&self) -> bool {
        self.deferred == Some(true)
    }

    pub fn is_directory(&self) -> bool {
        self.install.as_deref() == Some("directory")
    }

    /// The build for `target`: its own, else the sidecar's.
    pub fn build_for(&self, target: &Target) -> Option<Value> {
        target.build.clone().or_else(|| self.build.clone())
    }

    pub fn target(&self, triple: &str) -> Option<Target> {
        self.targets
            .get(triple)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    /// Where the program sits inside its install directory: `binaryInDir`
    /// for a tree, else the file name of what the build produces or of the
    /// archive member, else the sidecar's own name.
    pub fn program(&self, target: &Target) -> String {
        if self.is_directory() {
            return self.binary_in_dir.clone().unwrap_or_default();
        }
        let file_name = |path: &str| {
            Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        };
        let build = self.build_for(target);
        let produced = build
            .as_ref()
            .and_then(|build| build.get("producesPath"))
            .and_then(Value::as_str);
        let member = match target.archive.as_str() {
            "none" => None,
            _ => target.path_in_archive.as_deref(),
        };
        produced
            .or(member)
            .and_then(file_name)
            .unwrap_or_else(|| self.name.clone())
    }

    /// Record a hash for one target.
    pub fn pin(&mut self, triple: &str, sha256: &str) {
        if let Some(Value::Object(target)) = self.targets.get_mut(triple) {
            target.insert("sha256".to_string(), Value::String(sha256.to_string()));
        }
    }

    /// True once every declared target has a hash.
    pub fn fully_pinned(&self) -> bool {
        self.targets
            .keys()
            .all(|triple| self.target(triple).is_some_and(|t| !t.sha256.is_empty()))
    }
}

fn is_kebab(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

impl Manifest {
    pub fn read(path: &Path) -> Result<Self, ManifestError> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn write(&self, path: &Path) -> Result<(), ManifestError> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        Ok(std::fs::write(path, text)?)
    }

    /// Check every rule. With `require_hashes` off (recording mode), a
    /// missing hash is allowed; a malformed one never is.
    pub fn validate(&self, require_hashes: bool) -> Result<(), ManifestError> {
        let fail = |message: String| Err(ManifestError::Invalid(message));
        if let Some(minimum) = &self.minimum_macos {
            if crate::minos::MacVersion::parse(minimum).is_none() {
                return fail(format!(
                    "minimumMacos must be a macOS version such as 13.3, got {minimum:?}"
                ));
            }
        }
        if self.version != 1 {
            return fail(format!("unsupported manifest version: {}", self.version));
        }
        if self.out_dir.is_empty() {
            return fail("outDir must be a non-empty string".into());
        }
        let mut seen = BTreeSet::new();
        for sidecar in &self.sidecars {
            let name = &sidecar.name;
            if !is_kebab(name) {
                return fail(format!(
                    "sidecar name must be lowercase kebab-case: {name:?}"
                ));
            }
            if !seen.insert(name.clone()) {
                return fail(format!("duplicate sidecar: {name}"));
            }
            if sidecar.version.is_empty() {
                return fail(format!("{name}: version is required (pin it)"));
            }
            if sidecar.license.is_empty() {
                return fail(format!("{name}: license is required"));
            }
            match sidecar.install.as_deref() {
                None | Some("file") | Some("directory") => {}
                Some(other) => {
                    return fail(format!(
                        "{name}: install must be file or directory, got {other:?}"
                    ))
                }
            }
            if sidecar.is_directory() && sidecar.binary_in_dir.as_deref().unwrap_or("").is_empty() {
                return fail(format!(
                    "{name}: binaryInDir is required when install is \"directory\""
                ));
            }
            if !sidecar.is_directory() && sidecar.binary_in_dir.is_some() {
                return fail(format!(
                    "{name}: binaryInDir only applies when install is \"directory\""
                ));
            }
            if !sidecar.is_directory() && sidecar.keep_in_bin.is_some() {
                return fail(format!(
                    "{name}: keepInBin only applies when install is \"directory\""
                ));
            }
            if let Some(build) = &sidecar.build {
                check_build(name, build)?;
            }
            for required in APPLE_SLICES {
                if !sidecar.targets.contains_key(required) {
                    return fail(format!(
                        "{name}: missing {required} - the macOS build is universal (Intel + Apple Silicon)"
                    ));
                }
            }
            if sidecar.targets.contains_key(UNIVERSAL_TRIPLE) {
                return fail(format!(
                    "{name}: do not declare {UNIVERSAL_TRIPLE} - it is assembled from the two Apple slices"
                ));
            }
            for triple in sidecar.targets.keys() {
                let place = format!("{name}[{triple}]");
                let Some(target) = sidecar.target(triple) else {
                    return fail(format!("{place}: needs url and archive"));
                };
                if !target.url.starts_with("https://") {
                    return fail(format!("{place}: url must be https://"));
                }
                if !ARCHIVE_KINDS.contains(&target.archive.as_str()) {
                    return fail(format!(
                        "{place}: archive must be one of {}",
                        ARCHIVE_KINDS.join(", ")
                    ));
                }
                if target.archive == "none" && sidecar.is_directory() {
                    return fail(format!(
                        "{place}: a bare download cannot be a directory install"
                    ));
                }
                if let Some(build) = &target.build {
                    check_build(&place, build)?;
                }
                if target.archive == "none" && sidecar.build_for(&target).is_some() {
                    return fail(format!(
                        "{place}: a bare download cannot be built from source"
                    ));
                }
                if target.archive != "none"
                    && target.path_in_archive.as_deref().unwrap_or("").is_empty()
                {
                    return fail(format!("{place}: pathInArchive is required for archives"));
                }
                if target.sha256.is_empty() {
                    if require_hashes && !sidecar.is_draft() {
                        return fail(format!(
                            "{place}: sha256 is empty - run `cargo xtask fetch-sidecars --record`, check the value against the vendor's published checksums, then commit"
                        ));
                    }
                } else if !is_sha256(&target.sha256) {
                    return fail(format!(
                        "{place}: sha256 must be 64 lowercase hex characters"
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;

/// A build is a command run in the source directory, and the path it leaves
/// behind: the program for a file install, the installed tree for a
/// directory install.
fn check_build(place: &str, build: &Value) -> Result<(), ManifestError> {
    let fail = |message: String| Err(ManifestError::Invalid(message));
    let Some(build) = build.as_object() else {
        return fail(format!("{place}: build must be an object"));
    };
    let text = |key: &str| build.get(key).and_then(Value::as_str).unwrap_or("");
    if text("command").is_empty() {
        return fail(format!("{place}: build.command is required"));
    }
    if text("producesPath").is_empty() {
        return fail(format!(
            "{place}: build.producesPath is required - name what the build leaves behind"
        ));
    }
    Ok(())
}
