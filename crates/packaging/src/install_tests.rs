// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The download rules, with an archive built in memory and no network.

use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::json;

use super::*;

const X86: &str = "x86_64-apple-darwin";
const ARM: &str = "aarch64-apple-darwin";

/// A small distribution tree as a `.tar.gz`: two wanted programs, one
/// unwanted, and a data file.
fn archive(arch: &str) -> Vec<u8> {
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    let mut add = |path: &str, bytes: &[u8], mode: u32| {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes).unwrap();
    };
    add("pkg/bin/postgres", &macho::thin_header(arch), 0o644);
    add("pkg/bin/initdb", &macho::thin_header(arch), 0o755);
    add("pkg/bin/psql", b"client tool", 0o755);
    add("pkg/share/catalog", b"data", 0o644);
    tar.into_inner().unwrap().finish().unwrap()
}

struct Served(Vec<u8>, AtomicU32);

impl Downloader for Served {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(self.0.clone())
    }
}

fn manifest(sha_x86: &str, extra: serde_json::Value) -> Manifest {
    let target = |sha: &str| json!({ "url": "https://example.test/pg.tar.gz", "sha256": sha, "archive": "tar.gz", "pathInArchive": "pkg" });
    let mut sidecar = json!({
        "name": "postgres", "version": "1", "license": "PostgreSQL",
        "install": "directory", "binaryInDir": "bin/postgres", "keepInBin": ["postgres", "initdb"],
        "targets": { ARM: target(""), X86: target(sha_x86) }
    });
    for (key, value) in extra.as_object().unwrap() {
        sidecar[key] = value.clone();
    }
    serde_json::from_value(json!({ "version": 1, "outDir": "sidecars", "sidecars": [sidecar] }))
        .unwrap()
}

#[test]
fn installs_a_verified_tree_keeps_only_wanted_programs_and_is_then_current() {
    let root = tempfile::tempdir().unwrap();
    let payload = archive("x86_64");
    let mut manifest = manifest(&sha256_hex(&payload), json!({}));
    let served = Served(payload, AtomicU32::new(0));

    let first = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap();
    assert_eq!(first, Outcome::Installed);
    let dest = destination(root.path(), &manifest, "postgres", X86);
    assert!(dest.join("bin/postgres").is_file() && dest.join("bin/initdb").is_file());
    assert!(
        !dest.join("bin/psql").exists(),
        "unwanted program was bundled"
    );
    assert!(dest.join("share/catalog").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest.join("bin/postgres"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }

    let second = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap();
    assert_eq!(second, Outcome::AlreadyCurrent);
    assert_eq!(
        served.1.load(Ordering::SeqCst),
        1,
        "a current install must not download again"
    );
}

#[test]
fn a_checksum_mismatch_installs_nothing() {
    let root = tempfile::tempdir().unwrap();
    let mut manifest = manifest(&"0".repeat(64), json!({}));
    let served = Served(archive("x86_64"), AtomicU32::new(0));
    let error = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap_err();
    assert!(
        matches!(error, InstallError::ChecksumMismatch { .. }),
        "{error}"
    );
    assert!(!destination(root.path(), &manifest, "postgres", X86).exists());
}

#[test]
fn an_unpinned_target_fails_closed_and_recording_pins_it() {
    let root = tempfile::tempdir().unwrap();
    let payload = archive("arm64");
    let mut manifest = manifest(&"0".repeat(64), json!({}));
    let served = Served(payload.clone(), AtomicU32::new(0));

    let refused = install_one(root.path(), &mut manifest, 0, ARM, false, &served).unwrap_err();
    assert!(matches!(refused, InstallError::Unpinned(_)));
    assert_eq!(
        served.1.load(Ordering::SeqCst),
        0,
        "nothing may be downloaded without a pin"
    );

    let recorded = install_one(root.path(), &mut manifest, 0, ARM, true, &served).unwrap();
    assert_eq!(recorded, Outcome::Recorded(sha256_hex(&payload)));
    assert_eq!(
        manifest.sidecars[0].target(ARM).unwrap().sha256,
        sha256_hex(&payload)
    );
}

#[test]
fn the_wrong_architecture_fails_on_install_and_again_on_a_cache_hit() {
    let root = tempfile::tempdir().unwrap();
    let payload = archive("arm64"); // an Apple Silicon build pinned for Intel
    let mut manifest = manifest(&sha256_hex(&payload), json!({}));
    let served = Served(payload, AtomicU32::new(0));

    let fresh = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap_err();
    assert!(
        matches!(fresh, InstallError::WrongArchitecture { .. }),
        "{fresh}"
    );
    assert!(fresh.to_string().contains("expected x86_64, found arm64"));

    let cached = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap_err();
    assert!(matches!(cached, InstallError::WrongArchitecture { .. }));
    assert_eq!(served.1.load(Ordering::SeqCst), 1);
}

#[test]
fn draft_deferred_and_undeclared_targets_are_skipped_without_a_download() {
    let root = tempfile::tempdir().unwrap();
    let served = Served(archive("x86_64"), AtomicU32::new(0));
    let sha = "0".repeat(64);

    let mut draft = manifest(&sha, json!({ "draft": true }));
    assert!(matches!(
        install_one(root.path(), &mut draft, 0, X86, false, &served).unwrap(),
        Outcome::Skipped(_)
    ));
    let mut deferred = manifest(&sha, json!({ "deferred": true }));
    assert!(matches!(
        install_one(root.path(), &mut deferred, 0, X86, true, &served).unwrap(),
        Outcome::Skipped(_)
    ));
    let mut plain = manifest(&sha, json!({}));
    let other = install_one(
        root.path(),
        &mut plain,
        0,
        "x86_64-unknown-linux-gnu",
        false,
        &served,
    )
    .unwrap();
    assert!(matches!(other, Outcome::Skipped(_)));
    assert_eq!(served.1.load(Ordering::SeqCst), 0);
}

#[test]
fn a_wrong_path_in_the_archive_lists_what_is_there() {
    let root = tempfile::tempdir().unwrap();
    let payload = archive("x86_64");
    let mut manifest = manifest(
        &sha256_hex(&payload),
        json!({ "binaryInDir": "bin/server" }),
    );
    let served = Served(payload, AtomicU32::new(0));
    let error = install_one(root.path(), &mut manifest, 0, X86, false, &served)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("binaryInDir \"bin/server\" not found") && error.contains("bin/postgres"),
        "{error}"
    );
}
