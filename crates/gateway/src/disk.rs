// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Room on disk for a model download, checked before it starts so a first
//! download does not fill the disk and fail near the end.

use std::path::Path;

use isonomic_proto::{ErrorCode, WireError};

/// Space a download leaves free beyond the model itself: 1 GiB.
pub const DISK_HEADROOM: u64 = 1 << 30;

/// How much space the volume holding a directory has for this user.
pub trait DiskSpace: Send + Sync {
    /// Bytes available, or `None` when it cannot be read.
    fn available(&self, dir: &Path) -> Option<u64>;
}

/// The volume's own figure (`statvfs`: blocks available to this user).
pub struct SystemDisk;

impl DiskSpace for SystemDisk {
    fn available(&self, dir: &Path) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is a valid C string and `stat` a writable statvfs.
        if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
            return None;
        }
        #[allow(clippy::unnecessary_cast)] // The field widths differ by system.
        Some(stat.f_bavail as u64 * stat.f_frsize as u64)
    }
}

/// A fixed figure, for tests.
pub struct FixedDisk(pub u64);

impl DiskSpace for FixedDisk {
    fn available(&self, _: &Path) -> Option<u64> {
        Some(self.0)
    }
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// Whether `remaining` more bytes fit with the headroom to spare. A volume
/// that cannot be read is not refused: the download's own errors still
/// say what went wrong.
pub fn check(
    disk: &dyn DiskSpace,
    dir: &Path,
    name: &str,
    remaining: u64,
) -> Result<(), WireError> {
    let Some(available) = disk.available(dir) else {
        return Ok(());
    };
    let needed = remaining.saturating_add(DISK_HEADROOM);
    if available >= needed {
        return Ok(());
    }
    let message = format!(
        "There is not enough free disk space to download {name}: it needs {} and {} is free. \
         Free up some space, then try again.",
        gb(needed),
        gb(available)
    );
    Err(WireError::new(ErrorCode::TooLarge, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_needs_its_remaining_bytes_and_the_headroom() {
        let dir = Path::new("/");
        let fits = FixedDisk(DISK_HEADROOM + 10);
        assert!(check(&fits, dir, "Qwen3.5 2B", 10).is_ok());
        let e = check(&fits, dir, "Qwen3.5 2B", 11).unwrap_err();
        assert_eq!(e.code, ErrorCode::TooLarge);
        assert!(e
            .message
            .contains("not enough free disk space to download Qwen3.5 2B"));
        assert!(e.message.contains("Free up some space"), "{}", e.message);
    }

    #[test]
    fn the_real_volume_can_be_read() {
        let dir = tempfile::tempdir().unwrap();
        // Only that it can be read: the amount is the machine's.
        assert!(SystemDisk.available(dir.path()).is_some());
    }
}
