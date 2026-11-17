// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The product's minimum macOS, through `install_one`: a tree built from
//! source by one target's own build, and the refusal of anything in it that
//! says it needs a newer macOS. Archives are built in memory and the build
//! is a stand-in shell script.

use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::json;

use crate::install::{destination, install_one, sha256_hex, Downloader, InstallError, Outcome};
use crate::minos::{thin_with_minimum, MacVersion};

const X86: &str = "x86_64-apple-darwin";
const ARM: &str = "aarch64-apple-darwin";
const LINUX: &str = "x86_64-unknown-linux-gnu";

fn tar_gz(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (path, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
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

/// A source tree whose "build" installs prebuilt slices for `$ARCH` into
/// `isonomic-install/`, as `configure --prefix && make install` would; the
/// library says it needs `lib_minimum`.
fn source(lib_minimum: MacVersion) -> Vec<u8> {
    let mut entries = Vec::new();
    for arch in ["x86_64", "arm64"] {
        let ok = thin_with_minimum(arch, MacVersion(13, 3));
        entries.push((format!("pg-1/prebuilt/{arch}/postgres"), ok.clone()));
        entries.push((format!("pg-1/prebuilt/{arch}/initdb"), ok.clone()));
        entries.push((format!("pg-1/prebuilt/{arch}/psql"), ok));
        let lib = thin_with_minimum(arch, lib_minimum);
        entries.push((format!("pg-1/prebuilt/{arch}/libpq.dylib"), lib));
    }
    entries.push((
        "pg-1/COPYRIGHT".to_string(),
        b"Portions Copyright (c) the database's authors".to_vec(),
    ));
    let entries: Vec<(&str, Vec<u8>)> = entries
        .iter()
        .map(|(p, b)| (p.as_str(), b.clone()))
        .collect();
    tar_gz(&entries)
}

const SCRIPT: &str = "set -e; p=isonomic-install; mkdir -p $p/bin $p/lib $p/share; \
    cp prebuilt/$ARCH/postgres prebuilt/$ARCH/initdb prebuilt/$ARCH/psql $p/bin/; \
    cp prebuilt/$ARCH/libpq.dylib $p/lib/; echo data > $p/share/catalog";

fn manifest(sha: &str, minimum: Option<&str>) -> crate::manifest::Manifest {
    let built = json!({ "url": "https://example.test/pg-1.tar.gz", "sha256": sha,
        "archive": "tar.gz", "pathInArchive": "pg-1",
        "build": { "command": "sh", "args": ["-c", SCRIPT], "env": { "ARCH": "{arch}" },
                   "producesPath": "isonomic-install" } });
    let mut value = json!({ "version": 1, "outDir": "sidecars", "sidecars": [{
        "name": "postgres", "version": "1", "license": "PostgreSQL",
        "install": "directory", "binaryInDir": "bin/postgres", "keepInBin": ["postgres", "initdb"],
        "licenseFiles": ["LICENSE", "COPYRIGHT"],
        "targets": { ARM: built.clone(), X86: built, LINUX: {
            "url": "https://example.test/pg-linux.tar.gz", "sha256": sha,
            "archive": "tar.gz", "pathInArchive": "pg-1" } } }] });
    if let Some(minimum) = minimum {
        value["minimumMacos"] = json!(minimum);
    }
    let manifest: crate::manifest::Manifest = serde_json::from_value(value).unwrap();
    manifest.validate(true).unwrap();
    manifest
}

#[test]
fn a_tree_built_from_source_for_each_mac_installs_within_the_minimum() {
    let root = tempfile::tempdir().unwrap();
    let payload = source(MacVersion(13, 3));
    let mut manifest = manifest(&sha256_hex(&payload), Some("13.3"));
    let served = Served(payload, AtomicU32::new(0));
    for triple in [X86, ARM] {
        let done = install_one(root.path(), &mut manifest, 0, triple, false, &served).unwrap();
        assert_eq!(done, Outcome::Installed, "{triple}");
        let dest = destination(root.path(), &manifest, "postgres", triple);
        assert!(dest.join("bin/postgres").is_file() && dest.join("bin/initdb").is_file());
        assert!(
            !dest.join("bin/psql").exists(),
            "only the programs the engine runs"
        );
        assert!(dest.join("lib/libpq.dylib").is_file() && dest.join("share/catalog").is_file());
    }
}

#[test]
fn anything_needing_a_newer_macos_is_refused_and_not_left_installed() {
    let root = tempfile::tempdir().unwrap();
    let payload = source(MacVersion(26, 0));
    let mut manifest = manifest(&sha256_hex(&payload), Some("13.3"));
    let served = Served(payload, AtomicU32::new(0));
    let refused = install_one(root.path(), &mut manifest, 0, ARM, false, &served).unwrap_err();
    let InstallError::TooNewMacos { list, minimum, .. } = &refused else {
        panic!("{refused}");
    };
    assert_eq!(minimum, "13.3");
    assert!(
        list.contains("lib/libpq.dylib (arm64) needs macOS 26.0"),
        "{list}"
    );
    assert!(
        refused
            .to_string()
            .contains("needs a newer macOS than 13.3"),
        "{refused}"
    );
    assert!(!destination(root.path(), &manifest, "postgres", ARM).exists());
}

#[test]
fn without_a_stated_minimum_or_off_the_mac_nothing_is_checked() {
    let root = tempfile::tempdir().unwrap();
    let payload = source(MacVersion(26, 0));
    let served = Served(payload.clone(), AtomicU32::new(0));
    let mut unstated = manifest(&sha256_hex(&payload), None);
    let done = install_one(root.path(), &mut unstated, 0, X86, false, &served).unwrap();
    assert_eq!(done, Outcome::Installed);
    // The Linux target is a prebuilt tree under the same path, not built.
    let linux = tar_gz(&[
        ("pg-1/bin/postgres", b"elf".to_vec()),
        ("pg-1/bin/initdb", b"elf".to_vec()),
        ("pg-1/COPYRIGHT", b"text".to_vec()),
    ]);
    let served = Served(linux.clone(), AtomicU32::new(0));
    let mut stated = manifest(&sha256_hex(&linux), Some("13.3"));
    let done = install_one(root.path(), &mut stated, 0, LINUX, false, &served).unwrap();
    assert_eq!(done, Outcome::Installed);
}

#[test]
fn an_install_in_place_that_needs_a_newer_macos_is_built_again() {
    let root = tempfile::tempdir().unwrap();
    let payload = source(MacVersion(13, 3));
    let mut manifest = manifest(&sha256_hex(&payload), Some("13.3"));
    let served = Served(payload, AtomicU32::new(0));
    install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap();
    // An earlier recipe left a library that needs a newer macOS.
    let dest = destination(root.path(), &manifest, "postgres", X86);
    std::fs::write(
        dest.join("lib/libpq.dylib"),
        thin_with_minimum("x86_64", MacVersion(15, 0)),
    )
    .unwrap();
    let again = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap();
    assert_eq!(
        again,
        Outcome::Installed,
        "built again, not kept as current"
    );
    assert_eq!(served.1.load(Ordering::SeqCst), 2);
    let now = crate::minos::minimums(&dest.join("lib/libpq.dylib")).unwrap();
    assert_eq!(now, [("x86_64".to_string(), MacVersion(13, 3))]);
}

#[test]
fn a_build_is_repeated_only_when_its_source_or_recipe_changes() {
    let root = tempfile::tempdir().unwrap();
    let payload = source(MacVersion(13, 3));
    let mut manifest = manifest(&sha256_hex(&payload), Some("13.3"));
    let served = Served(payload, AtomicU32::new(0));
    install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap();
    let again = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap();
    assert_eq!(again, Outcome::AlreadyCurrent, "the same source and recipe");
    assert_eq!(served.1.load(Ordering::SeqCst), 1);
    let stamp = std::fs::read_to_string(
        destination(root.path(), &manifest, "postgres", X86).join(crate::install::STAMP),
    )
    .unwrap();
    assert!(stamp.contains(" recipe:"), "{stamp}");
    // A changed option is a new build.
    let target = manifest.sidecars[0].targets.get_mut(X86).unwrap();
    target["build"]["env"]["EXTRA"] = json!("1");
    let rebuilt = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap();
    assert_eq!(rebuilt, Outcome::Installed);
    assert_eq!(served.1.load(Ordering::SeqCst), 2);
}

#[test]
fn the_licence_is_kept_and_the_notice_names_every_bundled_program() {
    let root = tempfile::tempdir().unwrap();
    let payload = source(MacVersion(13, 3));
    let mut manifest = manifest(&sha256_hex(&payload), Some("13.3"));
    let served = Served(payload, AtomicU32::new(0));
    install_one(root.path(), &mut manifest, 0, ARM, false, &served).unwrap();
    let dest = destination(root.path(), &manifest, "postgres", ARM);
    let kept = std::fs::read_to_string(dest.join(crate::install::LICENSE)).unwrap();
    assert!(
        kept.contains("Copyright (c) the database's authors"),
        "{kept}"
    );

    let (path, missing) = crate::notices::write_notice(root.path(), &manifest).unwrap();
    assert!(missing.is_empty(), "{missing:?}");
    assert!(path.ends_with("sidecars/NOTICES.txt"));
    let notice = std::fs::read_to_string(path).unwrap();
    assert!(notice.starts_with("Isonomic includes the following programs"));
    assert!(notice.contains("postgres 1 (PostgreSQL)"), "{notice}");
    assert!(
        notice.contains("Copyright (c) the database's authors"),
        "{notice}"
    );
}

#[test]
fn an_archive_without_its_licence_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let payload = tar_gz(&[("pg-1/prebuilt/x86_64/postgres", b"x".to_vec())]);
    let mut manifest = manifest(&sha256_hex(&payload), None);
    let served = Served(payload, AtomicU32::new(0));
    let refused = install_one(root.path(), &mut manifest, 0, X86, false, &served).unwrap_err();
    let text = refused.to_string();
    assert!(text.contains("none of the licence files"), "{text}");
    assert!(
        text.contains("prebuilt/x86_64/postgres"),
        "says what is there: {text}"
    );
}

#[test]
fn an_install_from_before_its_licence_was_named_is_redone_and_a_missing_one_is_named() {
    let root = tempfile::tempdir().unwrap();
    let payload = source(MacVersion(13, 3));
    let mut manifest = manifest(&sha256_hex(&payload), Some("13.3"));
    let served = Served(payload, AtomicU32::new(0));
    install_one(root.path(), &mut manifest, 0, ARM, false, &served).unwrap();
    let dest = destination(root.path(), &manifest, "postgres", ARM);
    // As an install made before the manifest named its licence files.
    std::fs::remove_file(dest.join(crate::install::LICENSE)).unwrap();
    let (_, missing) = crate::notices::notice(root.path(), &manifest);
    assert_eq!(
        missing,
        ["postgres"],
        "an installed program without its licence"
    );

    let again = install_one(root.path(), &mut manifest, 0, ARM, false, &served).unwrap();
    assert_eq!(again, Outcome::Installed, "not current without its licence");
    assert!(dest.join(crate::install::LICENSE).is_file());
    let (_, missing) = crate::notices::notice(root.path(), &manifest);
    assert!(missing.is_empty(), "{missing:?}");
}
