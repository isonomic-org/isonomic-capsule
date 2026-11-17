// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The oldest macOS each Mach-O file says it runs on, so nothing that needs
//! a newer macOS than the product's minimum is ever bundled.
//!
//! The minimum is the `minos` of a file's `LC_BUILD_VERSION` load command
//! (platform macOS), or the version in `LC_VERSION_MIN_MACOSX` in older
//! files. A compiler writes the macOS it targets there; without a
//! deployment target, that is the macOS of the Mac it ran on, which is how
//! a binary ends up saying it needs a newer macOS than anyone intended.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MH_MAGIC: u32 = 0xfeed_face;
const MH_MAGIC_64: u32 = 0xfeed_facf;
const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const LC_BUILD_VERSION: u32 = 0x32;
const LC_VERSION_MIN_MACOSX: u32 = 0x24;
const PLATFORM_MACOS: u32 = 1;
/// No real header has more load commands than this.
const MAX_COMMAND_BYTES: u32 = 1 << 20;

/// A macOS version, major and minor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MacVersion(pub u32, pub u32);

impl MacVersion {
    /// `13.3`, `26.0` or `15`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().map_or(Some(0), |m| m.parse().ok())?;
        Some(Self(major, minor))
    }

    fn decode(packed: u32) -> Self {
        Self(packed >> 16, (packed >> 8) & 0xff)
    }
}

impl std::fmt::Display for MacVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.0, self.1)
    }
}

/// One file slice that needs a newer macOS than allowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TooNew {
    pub path: PathBuf,
    pub arch: String,
    pub needs: MacVersion,
}

fn u32_at(bytes: &[u8], at: usize, big: bool) -> Option<u32> {
    let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(if big {
        u32::from_be_bytes(b)
    } else {
        u32::from_le_bytes(b)
    })
}

fn arch_name(cpu: u32) -> String {
    match cpu {
        0x0100_0007 => "x86_64".into(),
        0x0100_000c => "arm64".into(),
        other => format!("cputype:{other}"),
    }
}

/// The minimum of the thin Mach-O starting at `offset` in `file`.
fn slice_minimum(
    file: &mut std::fs::File,
    offset: u64,
) -> std::io::Result<Option<(String, MacVersion)>> {
    let mut head = [0u8; 32];
    file.seek(SeekFrom::Start(offset))?;
    if file.read(&mut head)? < 28 {
        return Ok(None);
    }
    let header_bytes = match u32_at(&head, 0, false) {
        Some(MH_MAGIC_64) => 32,
        Some(MH_MAGIC) => 28,
        _ => return Ok(None),
    };
    let arch = arch_name(u32_at(&head, 4, false).unwrap_or(0));
    let ncmds = u32_at(&head, 16, false).unwrap_or(0);
    let size = u32_at(&head, 20, false).unwrap_or(0).min(MAX_COMMAND_BYTES);
    let mut commands = vec![0u8; size as usize];
    file.seek(SeekFrom::Start(offset + header_bytes))?;
    file.read_exact(&mut commands)?;
    let mut at = 0usize;
    for _ in 0..ncmds {
        let (Some(cmd), Some(len)) = (
            u32_at(&commands, at, false),
            u32_at(&commands, at + 4, false),
        ) else {
            break;
        };
        match cmd {
            LC_BUILD_VERSION if u32_at(&commands, at + 8, false) == Some(PLATFORM_MACOS) => {
                return Ok(u32_at(&commands, at + 12, false).map(|v| (arch, MacVersion::decode(v))));
            }
            LC_VERSION_MIN_MACOSX => {
                return Ok(u32_at(&commands, at + 8, false).map(|v| (arch, MacVersion::decode(v))));
            }
            _ => {}
        }
        if len == 0 {
            break;
        }
        at += len as usize;
    }
    Ok(None)
}

/// Each slice's architecture and minimum. Empty when the file is not Mach-O
/// or states no macOS minimum.
pub fn minimums(path: &Path) -> std::io::Result<Vec<(String, MacVersion)>> {
    let mut file = std::fs::File::open(path)?;
    let mut head = [0u8; 8];
    if file.read(&mut head)? < 8 {
        return Ok(Vec::new());
    }
    let magic = u32_at(&head, 0, true).unwrap_or(0);
    if magic != FAT_MAGIC && magic != FAT_MAGIC_64 {
        return Ok(slice_minimum(&mut file, 0)?.into_iter().collect());
    }
    let wide = magic == FAT_MAGIC_64;
    let count = u32_at(&head, 4, true).unwrap_or(0).min(64);
    let stride = if wide { 32 } else { 20 };
    let mut table = vec![0u8; count as usize * stride];
    file.read_exact(&mut table)?;
    let mut found = Vec::new();
    for i in 0..count as usize {
        let at = i * stride;
        let offset = if wide {
            let hi = u32_at(&table, at + 8, true).unwrap_or(0) as u64;
            (hi << 32) | u32_at(&table, at + 12, true).unwrap_or(0) as u64
        } else {
            u32_at(&table, at + 8, true).unwrap_or(0) as u64
        };
        found.extend(slice_minimum(&mut file, offset)?);
    }
    Ok(found)
}

/// Every file slice under `tree` that needs a newer macOS than `allowed`.
pub fn too_new(tree: &Path, allowed: MacVersion) -> std::io::Result<Vec<TooNew>> {
    let mut found = Vec::new();
    let mut stack = vec![tree.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) if dir == tree && tree.is_file() => {
                collect(tree, allowed, &mut found)?;
                continue;
            }
            Err(e) => return Err(e),
        };
        for entry in entries.flatten() {
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                collect(&entry.path(), allowed, &mut found)?;
            }
        }
    }
    found.sort_by(|a, b| (&a.path, &a.arch).cmp(&(&b.path, &b.arch)));
    Ok(found)
}

fn collect(path: &Path, allowed: MacVersion, found: &mut Vec<TooNew>) -> std::io::Result<()> {
    for (arch, needs) in minimums(path)? {
        if needs > allowed {
            found.push(TooNew {
                path: path.to_path_buf(),
                arch,
                needs,
            });
        }
    }
    Ok(())
}

/// A thin 64-bit Mach-O for `arch` that says it needs macOS `minos`
/// (tests and stand-ins).
pub fn thin_with_minimum(arch: &str, minos: MacVersion) -> Vec<u8> {
    let cpu: u32 = if arch == "arm64" {
        0x0100_000c
    } else {
        0x0100_0007
    };
    let packed = (minos.0 << 16) | (minos.1 << 8);
    let mut bytes = Vec::new();
    for word in [MH_MAGIC_64, cpu, 0, 2, 1, 24, 0, 0] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    for word in [LC_BUILD_VERSION, 24, PLATFORM_MACOS, packed, packed, 0] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// A universal file holding `slices` (tests and stand-ins).
pub fn universal(slices: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&FAT_MAGIC.to_be_bytes());
    out.extend_from_slice(&(slices.len() as u32).to_be_bytes());
    let mut offset = 4096u32;
    let mut body = Vec::new();
    for slice in slices {
        let cpu = u32_at(slice, 4, false).unwrap_or(0);
        for word in [cpu, 0, offset, slice.len() as u32, 12] {
            out.extend_from_slice(&word.to_be_bytes());
        }
        body.push((offset, slice.clone()));
        offset += 4096;
    }
    for (at, slice) in body {
        out.resize(at as usize, 0);
        out.extend_from_slice(&slice);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(MacVersion::parse("13.3"), Some(MacVersion(13, 3)));
        assert_eq!(MacVersion::parse("26"), Some(MacVersion(26, 0)));
        assert!(MacVersion(15, 0) > MacVersion(13, 3));
        assert_eq!(MacVersion::parse("x"), None);
    }

    #[test]
    fn a_thin_and_a_universal_file_say_their_minimums() {
        let dir = tempfile::tempdir().unwrap();
        let thin = file(
            dir.path(),
            "thin",
            &thin_with_minimum("x86_64", MacVersion(13, 3)),
        );
        assert_eq!(
            minimums(&thin).unwrap(),
            [("x86_64".into(), MacVersion(13, 3))]
        );
        let both = universal(&[
            thin_with_minimum("x86_64", MacVersion(13, 0)),
            thin_with_minimum("arm64", MacVersion(26, 0)),
        ]);
        let fat = file(dir.path(), "fat", &both);
        assert_eq!(
            minimums(&fat).unwrap(),
            [
                ("x86_64".into(), MacVersion(13, 0)),
                ("arm64".into(), MacVersion(26, 0))
            ]
        );
        let text = file(dir.path(), "README", b"not a binary");
        assert!(minimums(&text).unwrap().is_empty());
    }

    #[test]
    fn every_slice_above_the_minimum_is_found_in_a_tree() {
        let dir = tempfile::tempdir().unwrap();
        file(
            dir.path(),
            "bin/ok",
            &thin_with_minimum("arm64", MacVersion(13, 3)),
        );
        file(
            dir.path(),
            "lib/new.dylib",
            &thin_with_minimum("arm64", MacVersion(26, 0)),
        );
        file(dir.path(), "share/notes.txt", b"text");
        let found = too_new(dir.path(), MacVersion(13, 3)).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            (found[0].arch.as_str(), found[0].needs),
            ("arm64", MacVersion(26, 0))
        );
        assert!(found[0].path.ends_with("lib/new.dylib"));
    }
}
