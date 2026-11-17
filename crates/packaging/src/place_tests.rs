// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The file and build shapes, end to end through `install_one`, with
//! archives built in memory and a stand-in build command.

use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::json;

use super::arch_name;
use crate::install::{destination, install_one, sha256_hex, Downloader, InstallError, Outcome};
use crate::macho;
use crate::manifest::Manifest;

const X86: &str = "x86_64-apple-darwin";
const ARM: &str = "aarch64-apple-darwin";

fn tar_gz(entries: &[(&str, Vec<u8>, u32)]) -> Vec<u8> {
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (path, bytes, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(*mode);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

struct Served(Vec<u8>, AtomicU32);

impl Downloader for Served {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(self.0.clone())
    }
}

fn one(sidecar: serde_json::Value) -> Manifest {
    serde_json::from_value(json!({ "version": 1, "outDir": "sidecars", "sidecars": [sidecar] }))
        .unwrap()
}

fn file_sidecar(sha: &str) -> Manifest {
    let target = json!({ "url": "https://example.test/objects.tar.gz", "sha256": sha,
                         "archive": "tar.gz", "pathInArchive": "weed" });
    one(
        json!({ "name": "objects", "version": "1", "license": "Apache-2.0",
                "targets": { ARM: target.clone(), X86: target } }),
    )
}

/// A source archive whose "build" copies the prebuilt slice for `$ARCH`.
fn source() -> Vec<u8> {
    tar_gz(&[
        ("src-1/out-x86_64", macho::thin_header("x86_64"), 0o644),
        ("src-1/out-arm64", macho::thin_header("arm64"), 0o644),
        ("src-1/README", b"source".to_vec(), 0o644),
    ])
}

fn build_sidecar(sha: &str, script: &str) -> Manifest {
    let target = json!({ "url": "https://example.test/cache-1.tar.gz", "sha256": sha,
                         "archive": "tar.gz", "pathInArchive": "src-1" });
    one(
        json!({ "name": "cache", "version": "1", "license": "BSD-3-Clause",
                "build": { "command": "sh", "args": ["-c", script],
                           "env": { "ARCH": "{arch}" }, "producesPath": "target/cache-server" },
                "targets": { ARM: target.clone(), X86: target } }),
    )
}

#[test]
fn arch_names_follow_the_toolchains() {
    assert_eq!(arch_name(ARM), "arm64");
    assert_eq!(arch_name(X86), "x86_64");
    assert_eq!(arch_name("aarch64-unknown-linux-gnu"), "aarch64");
}

#[test]
fn a_file_install_keeps_one_executable_program_and_is_then_current() {
    let root = tempfile::tempdir().unwrap();
    let payload = tar_gz(&[
        ("weed", macho::thin_header("x86_64"), 0o644),
        ("LICENSE", b"text".to_vec(), 0o644),
    ]);
    let mut manifest = file_sidecar(&sha256_hex(&payload));
    let served = Served(payload, AtomicU32::new(0));

    let first = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap();
    assert_eq!(first, Outcome::Installed);
    let dest = destination(root.path(), &manifest, "objects", X86);
    assert!(dest.join("weed").is_file());
    assert!(!dest.join("LICENSE").exists(), "only the program is kept");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest.join("weed"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
    let again = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap();
    assert_eq!(again, Outcome::AlreadyCurrent);
    assert_eq!(served.1.load(Ordering::SeqCst), 1);
}

#[test]
fn a_file_built_for_the_other_mac_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let payload = tar_gz(&[("weed", macho::thin_header("arm64"), 0o755)]);
    let mut manifest = file_sidecar(&sha256_hex(&payload));
    let served = Served(payload, AtomicU32::new(0));
    let error = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap_err();
    assert!(
        matches!(error, InstallError::WrongArchitecture { .. }),
        "{error}"
    );
}

#[test]
fn a_bare_download_is_the_program() {
    let root = tempfile::tempdir().unwrap();
    let payload = macho::thin_header("arm64");
    let target = json!({ "url": "https://example.test/tool", "sha256": sha256_hex(&payload),
                         "archive": "none" });
    let mut manifest = one(json!({ "name": "tool", "version": "1", "license": "MIT",
                                   "targets": { ARM: target.clone(), X86: target } }));
    let served = Served(payload, AtomicU32::new(0));
    let done = install_one(root.path(), &mut manifest, 0, ARM, false, &served).unwrap();
    assert_eq!(done, Outcome::Installed);
    assert!(destination(root.path(), &manifest, "tool", ARM)
        .join("tool")
        .is_file());
}

#[cfg(unix)]
#[test]
fn a_build_runs_per_target_with_the_arch_filled_in_and_keeps_its_log() {
    let root = tempfile::tempdir().unwrap();
    let payload = source();
    let script = "mkdir -p target && cp out-$ARCH target/cache-server && echo built for $ARCH";
    let mut manifest = build_sidecar(&sha256_hex(&payload), script);
    let served = Served(payload, AtomicU32::new(0));

    for triple in [X86, ARM] {
        let done = install_one(root.path(), &mut manifest, 0, triple, false, &served).unwrap();
        assert_eq!(done, Outcome::Installed, "{triple}");
        let dest = destination(root.path(), &manifest, "cache", triple);
        assert!(dest.join("cache-server").is_file());
        let log = root
            .path()
            .join(format!("sidecars/.cache/cache-{triple}.build.log"));
        let text = std::fs::read_to_string(log).unwrap();
        assert!(
            text.contains(&format!("built for {}", arch_name(triple))),
            "{text}"
        );
    }
    let again = install_one(root.path(), &mut manifest, 0, ARM, false, &served).unwrap();
    assert_eq!(again, Outcome::AlreadyCurrent);
}

#[cfg(unix)]
#[test]
fn a_failed_build_installs_nothing_and_shows_the_end_of_its_log() {
    let root = tempfile::tempdir().unwrap();
    let payload = source();
    let mut manifest = build_sidecar(&sha256_hex(&payload), "echo compiler says no >&2; exit 3");
    let served = Served(payload, AtomicU32::new(0));
    let error = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap_err();
    let text = error.to_string();
    assert!(matches!(error, InstallError::Build { .. }), "{text}");
    assert!(text.contains("compiler says no"), "{text}");
    assert!(!destination(root.path(), &manifest, "cache", X86).exists());
}

#[cfg(unix)]
#[test]
fn a_build_that_produces_nothing_says_what_it_found() {
    let root = tempfile::tempdir().unwrap();
    let payload = source();
    let mut manifest = build_sidecar(&sha256_hex(&payload), "true");
    let served = Served(payload, AtomicU32::new(0));
    let error = install_one(root.path(), &mut manifest, 0, X86, false, &served)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("target/cache-server") && error.contains("README"),
        "{error}"
    );
}
