// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which architectures a Mach-O file contains, read from its first bytes.
//!
//! A binary built for the wrong Mac is invisible until a user launches it, so
//! every installed artifact is checked against the architecture its target
//! requires. Only the header is read, which is cheap enough to run on every
//! install, including ones already in place.

use std::io::Read;
use std::path::Path;

/// Bytes to read from the start of a file: enough for a universal header.
pub const HEADER_BYTES: usize = 4096;

const MH_MAGIC: u32 = 0xfeed_face;
const MH_MAGIC_64: u32 = 0xfeed_facf;
const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const FAT_ENTRY_BYTES: usize = 20;
const FAT_64_ENTRY_BYTES: usize = 32;

fn cpu_name(cpu_type: u32) -> String {
    match cpu_type {
        0x0100_0007 => "x86_64".to_string(),
        0x0100_000c => "arm64".to_string(),
        0x0000_0007 => "i386".to_string(),
        0x0000_000c => "arm".to_string(),
        other => format!("cputype:{other}"),
    }
}

fn be(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn le(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Architectures in a Mach-O header. Empty when the bytes are not Mach-O.
pub fn archs(header: &[u8]) -> Vec<String> {
    if header.len() < 8 {
        return Vec::new();
    }
    let leading = be(header, 0);
    if leading == FAT_MAGIC || leading == FAT_MAGIC_64 {
        let stride = if leading == FAT_MAGIC_64 {
            FAT_64_ENTRY_BYTES
        } else {
            FAT_ENTRY_BYTES
        };
        let count = be(header, 4) as usize;
        let mut found = Vec::new();
        for index in 0..count {
            let offset = 8 + index * stride;
            if offset + 4 > header.len() {
                break; // a short read, not a short file
            }
            found.push(cpu_name(be(header, offset)));
        }
        return found;
    }
    let little = le(header, 0);
    if little == MH_MAGIC || little == MH_MAGIC_64 {
        return vec![cpu_name(le(header, 4))];
    }
    if leading == MH_MAGIC || leading == MH_MAGIC_64 {
        return vec![cpu_name(be(header, 4))];
    }
    Vec::new()
}

/// Architectures a target triple requires. Empty for non-Apple targets,
/// which this check does not cover.
pub fn required(triple: &str) -> Vec<&'static str> {
    if triple == "universal-apple-darwin" {
        vec!["arm64", "x86_64"]
    } else if triple.starts_with("aarch64-apple") {
        vec!["arm64"]
    } else if triple.starts_with("x86_64-apple") {
        vec!["x86_64"]
    } else {
        Vec::new()
    }
}

/// `None` when the file suits the target; otherwise what is wrong.
pub fn problem(found: &[String], triple: &str) -> Option<String> {
    let required = required(triple);
    if required.is_empty() || found.is_empty() {
        return None; // not an Apple target, or not Mach-O: nothing to assert
    }
    let missing = required.iter().any(|arch| !found.iter().any(|f| f == arch));
    missing.then(|| {
        format!(
            "expected {}, found {}",
            required.join(" + "),
            found.join(", ")
        )
    })
}

/// Check a file on disk against a target triple.
pub fn check_file(path: &Path, triple: &str) -> std::io::Result<Option<String>> {
    let mut header = vec![0u8; HEADER_BYTES];
    let mut file = std::fs::File::open(path)?;
    let mut read = 0;
    while read < header.len() {
        match file.read(&mut header[read..])? {
            0 => break,
            n => read += n,
        }
    }
    Ok(problem(&archs(&header[..read]), triple))
}

/// A minimal thin 64-bit Mach-O header for `arch` (tests and stand-ins).
pub fn thin_header(arch: &str) -> Vec<u8> {
    let cpu: u32 = if arch == "arm64" {
        0x0100_000c
    } else {
        0x0100_0007
    };
    let mut bytes = MH_MAGIC_64.to_le_bytes().to_vec();
    bytes.extend_from_slice(&cpu.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 24]);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fat(cpus: &[u32]) -> Vec<u8> {
        let mut bytes = FAT_MAGIC.to_be_bytes().to_vec();
        bytes.extend_from_slice(&(cpus.len() as u32).to_be_bytes());
        for cpu in cpus {
            bytes.extend_from_slice(&cpu.to_be_bytes());
            bytes.extend_from_slice(&[0u8; 16]);
        }
        bytes
    }

    #[test]
    fn reads_thin_and_universal_headers() {
        assert_eq!(archs(&thin_header("x86_64")), ["x86_64"]);
        assert_eq!(archs(&thin_header("arm64")), ["arm64"]);
        assert_eq!(
            archs(&fat(&[0x0100_0007, 0x0100_000c])),
            ["x86_64", "arm64"]
        );
        assert!(archs(b"#!/bin/sh\necho hi\n").is_empty());
        assert!(archs(&[0xcf, 0xfa]).is_empty());
    }

    #[test]
    fn a_wrong_or_missing_slice_is_a_problem_and_other_cases_are_not() {
        let x86 = archs(&thin_header("x86_64"));
        assert_eq!(problem(&x86, "x86_64-apple-darwin"), None);
        assert_eq!(
            problem(&x86, "aarch64-apple-darwin").as_deref(),
            Some("expected arm64, found x86_64")
        );
        assert!(problem(&x86, "universal-apple-darwin").is_some());
        assert_eq!(
            problem(
                &archs(&fat(&[0x0100_0007, 0x0100_000c])),
                "universal-apple-darwin"
            ),
            None
        );
        // Not Mach-O, or not an Apple target: outside this check.
        assert_eq!(problem(&[], "x86_64-apple-darwin"), None);
        assert_eq!(problem(&x86, "x86_64-unknown-linux-gnu"), None);
    }

    #[test]
    fn checks_a_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin");
        std::fs::write(&path, thin_header("arm64")).unwrap();
        assert_eq!(check_file(&path, "aarch64-apple-darwin").unwrap(), None);
        assert!(check_file(&path, "x86_64-apple-darwin").unwrap().is_some());
    }
}
