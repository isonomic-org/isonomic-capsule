//! Download, verify and install one pinned artifact.
//!
//! The order is fixed: nothing is unpacked before its SHA-256 matches the
//! pin, and nothing is reported installed before its architecture matches
//! its target. An artifact already in place is still architecture-checked:
//! an install left by an earlier, buggier run is exactly what that catches.
//!
//! Directory installs are implemented. The single-file and build-from-source
//! shapes validate in the manifest and are refused here until a service
//! that needs them is bundled.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::macho;
use crate::manifest::Manifest;

/// Fetches a URL. The real one is HTTP; tests serve bytes from memory.
pub trait Downloader {
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Written inside an installed directory: the hash of what was downloaded.
pub const STAMP: &str = ".sha256";

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
    #[error("{0}: this install shape is not implemented yet (only directory installs are)")]
    UnsupportedShape(String),
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

fn verify_arch(binary: &Path, triple: &str, label: &str) -> Result<(), InstallError> {
    match macho::check_file(binary, triple)? {
        Some(problem) => Err(InstallError::WrongArchitecture {
            label: label.to_string(),
            problem,
            path: binary.to_path_buf(),
        }),
        None => Ok(()),
    }
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

fn contents(dir: &Path) -> String {
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
    if !sidecar.is_directory() || sidecar.build.is_some() {
        return Err(InstallError::UnsupportedShape(label));
    }
    let binary_in_dir = sidecar.binary_in_dir.clone().unwrap_or_default();
    let dest = destination(root, manifest, &sidecar.name, triple);
    let pinned = target.sha256.clone();
    if pinned.is_empty() && !record {
        return Err(InstallError::Unpinned(label));
    }

    if !pinned.is_empty() {
        let stamped = std::fs::read_to_string(dest.join(STAMP))
            .map(|s| s.trim() == pinned)
            .unwrap_or(false);
        if stamped {
            verify_arch(&dest.join(&binary_in_dir), triple, &label)?;
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

    // Unpack into the cache, then move the wanted directory into place.
    let work = root
        .join(&manifest.out_dir)
        .join(".cache")
        .join(format!("{}-{triple}", sidecar.name));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let unpack = tar::Archive::new(flate2::read::GzDecoder::new(payload.as_slice())).unpack(&work);
    unpack
        .map_err(|error| InstallError::Archive(label.clone(), format!("cannot unpack: {error}")))?;
    let member = work.join(target.path_in_archive.as_deref().unwrap_or_default());
    if !member.is_dir() {
        let found = contents(&work);
        let wanted = target.path_in_archive.unwrap_or_default();
        return Err(InstallError::Archive(
            label,
            format!("pathInArchive \"{wanted}\" is not a directory. Archive contains:\n  {found}"),
        ));
    }
    if !member.join(&binary_in_dir).is_file() {
        let found = contents(&member);
        return Err(InstallError::Archive(
            label,
            format!("binaryInDir \"{binary_in_dir}\" not found. Directory contains:\n  {found}"),
        ));
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&member, &dest)?;
    let _ = std::fs::remove_dir_all(&work);

    // Bundle only the programs the engine runs.
    if let Some(keep) = &sidecar.keep_in_bin {
        for entry in std::fs::read_dir(dest.join("bin"))?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !keep.contains(&name) {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dest.join(&binary_in_dir),
            std::fs::Permissions::from_mode(0o755),
        )?;
    }
    std::fs::write(dest.join(STAMP), format!("{actual}\n"))?;
    verify_arch(&dest.join(&binary_in_dir), triple, &label)?;
    Ok(if recorded {
        Outcome::Recorded(actual)
    } else {
        Outcome::Installed
    })
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
