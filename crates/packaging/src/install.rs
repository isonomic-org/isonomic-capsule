// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Download, verify and install one pinned artifact.
//!
//! The order is fixed: nothing is unpacked before its SHA-256 matches the
//! pin, and nothing is reported installed before its architecture matches
//! its target. An artifact already in place is still architecture-checked:
//! an install left by an earlier, buggier run is exactly what that catches.
//!
//! Three shapes, placed by `place`: a directory tree, one program taken out
//! of an archive, or one program built from a verified source archive. Each
//! lands in `<outDir>/<name>-<triple>/` with a stamp of the downloaded hash.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::macho;
use crate::manifest::Manifest;

/// Fetches a URL. The real one is HTTP; tests serve bytes from memory.
pub trait Downloader {
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Written inside an installed directory: the hash of what was downloaded,
/// and for a build the digest of its recipe, so a build is repeated only
/// when its source or its options change.
pub const STAMP: &str = ".sha256";

/// Written inside an installed directory: the program's licence text, for
/// the bundle's notice (`notices`).
pub const LICENSE: &str = ".license";

/// What an install's stamp says: the download's hash, then the recipe's
/// digest when it is built.
fn stamp(
    sidecar: &crate::manifest::Sidecar,
    target: &crate::manifest::Target,
    sha: &str,
) -> String {
    match sidecar.build_for(target) {
        Some(build) => {
            let recipe = serde_json::to_string(&build).unwrap_or_default();
            format!("{sha} recipe:{}", sha256_hex(recipe.as_bytes()))
        }
        None => sha.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not fetched, and why: draft, deferred, or no such target.
    Skipped(&'static str),
    AlreadyCurrent,
    Installed,
    /// Installed, and this hash was written into the manifest.
    Recorded(String),
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("{0} has no pinned sha256. Refusing to install an unverified binary. Run `cargo xtask fetch-sidecars --record` on a trusted machine, check the printed hash against the vendor's published checksums, and commit sidecars.json.")]
    Unpinned(String),
    #[error("{label} checksum mismatch.\n  expected {expected}\n  actual   {actual}\nRefusing to install. Either the pin is stale or the download was tampered with.")]
    ChecksumMismatch {
        label: String,
        expected: String,
        actual: String,
    },
    #[error("{label}: wrong architecture - {problem}.\n  {}\nA binary built for the wrong Mac is invisible until a user launches it.", path.display())]
    WrongArchitecture {
        label: String,
        problem: String,
        path: PathBuf,
    },
    #[error("{label}: the build failed ({status}). Last lines of {}:\n{tail}", log.display())]
    Build {
        label: String,
        status: String,
        log: PathBuf,
        tail: String,
    },
    #[error("{label} needs a newer macOS than {minimum}, the oldest the product supports, so it was not installed:\n{list}\nBuild it for macOS {minimum} (a deployment target), or pin a build that targets it.")]
    TooNewMacos {
        label: String,
        minimum: String,
        list: String,
    },
    #[error("{0}: {1}")]
    Archive(String, String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Where one artifact is installed.
pub fn destination(root: &Path, manifest: &Manifest, name: &str, triple: &str) -> PathBuf {
    root.join(&manifest.out_dir)
        .join(format!("{name}-{triple}"))
}

pub(crate) fn verify_arch(binary: &Path, triple: &str, label: &str) -> Result<(), InstallError> {
    match macho::check_file(binary, triple)? {
        Some(problem) => Err(InstallError::WrongArchitecture {
            label: label.to_string(),
            problem,
            path: binary.to_path_buf(),
        }),
        None => Ok(()),
    }
}

/// Refuse, and remove, an install with any file that needs a newer macOS
/// than the product's minimum. Non-Apple targets are not checked.
fn verify_minimum(
    manifest: &Manifest,
    dest: &Path,
    triple: &str,
    label: &str,
) -> Result<(), InstallError> {
    let Some(minimum) = manifest.minimum_macos.as_deref() else {
        return Ok(());
    };
    let Some(allowed) = crate::minos::MacVersion::parse(minimum) else {
        return Ok(()); // validation refuses a bad minimum before this
    };
    if !triple.ends_with("apple-darwin") {
        return Ok(());
    }
    let found = crate::minos::too_new(dest, allowed)?;
    if found.is_empty() {
        return Ok(());
    }
    let list = found
        .iter()
        .take(20)
        .map(|f| {
            let path = f.path.strip_prefix(dest).unwrap_or(&f.path);
            format!("  {} ({}) needs macOS {}", path.display(), f.arch, f.needs)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::remove_dir_all(dest);
    Err(InstallError::TooNewMacos {
        label: label.to_string(),
        minimum: minimum.to_string(),
        list,
    })
}

fn list_files(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() {
            list_files(&path, root, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.display().to_string());
        }
    }
}

pub(crate) fn contents(dir: &Path) -> String {
    let mut files = Vec::new();
    list_files(dir, dir, &mut files);
    files.sort();
    files.truncate(40);
    files.join("\n  ")
}

/// Install one sidecar for one target. `root` is the repo root.
pub fn install_one(
    root: &Path,
    manifest: &mut Manifest,
    index: usize,
    triple: &str,
    record: bool,
    downloader: &dyn Downloader,
) -> Result<Outcome, InstallError> {
    let sidecar = manifest.sidecars[index].clone();
    let label = format!("{}[{triple}]", sidecar.name);
    if sidecar.is_deferred() {
        return Ok(Outcome::Skipped(
            "deferred: pinned, not bundled until its milestone",
        ));
    }
    if sidecar.is_draft() && !record {
        return Ok(Outcome::Skipped("draft: declared, not yet pinned"));
    }
    let Some(target) = sidecar.target(triple) else {
        return Ok(Outcome::Skipped("no such target declared"));
    };
    let program = sidecar.program(&target);
    let dest = destination(root, manifest, &sidecar.name, triple);
    let pinned = target.sha256.clone();
    if pinned.is_empty() && !record {
        return Err(InstallError::Unpinned(label));
    }

    if !pinned.is_empty() {
        let expected = stamp(&sidecar, &target, &pinned);
        let stamped = std::fs::read_to_string(dest.join(STAMP))
            .map(|s| s.trim() == expected)
            .unwrap_or(false);
        // A tree in place that needs a newer macOS (built before the
        // recipe targeted the minimum, say) is not current: it is removed
        // here and installed again below.
        // One installed before the manifest named its licence files has no
        // licence text for the notice: it is installed again too.
        let licensed = sidecar.license_files.is_none() || dest.join(LICENSE).is_file();
        if stamped && licensed && verify_minimum(manifest, &dest, triple, &label).is_ok() {
            verify_arch(&dest.join(&program), triple, &label)?;
            return Ok(Outcome::AlreadyCurrent);
        }
    }

    let payload = downloader
        .get(&target.url)
        .map_err(InstallError::Download)?;
    let actual = sha256_hex(&payload);
    if !pinned.is_empty() && actual != pinned {
        return Err(InstallError::ChecksumMismatch {
            label,
            expected: pinned,
            actual,
        });
    }
    let recorded = record && pinned.is_empty();
    if recorded {
        manifest.sidecars[index].pin(triple, &actual);
    }

    let work = root
        .join(&manifest.out_dir)
        .join(".cache")
        .join(format!("{}-{triple}", sidecar.name));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let placed = crate::place::place(&sidecar, &target, triple, &label, &payload, &work, &dest);
    let _ = std::fs::remove_dir_all(&work);
    placed?;
    verify_minimum(manifest, &dest, triple, &label)?;
    std::fs::write(
        dest.join(STAMP),
        format!("{}\n", stamp(&sidecar, &target, &actual)),
    )?;
    verify_arch(&dest.join(&program), triple, &label)?;
    Ok(if recorded {
        Outcome::Recorded(actual)
    } else {
        Outcome::Installed
    })
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
