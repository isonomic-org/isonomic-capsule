// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a report says about the machine: the macOS version, read from the
//! system's own version file, and memory in whole gigabytes.

/// The macOS version, such as `15.1`; empty off the Mac.
pub fn macos_version() -> String {
    std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
        .ok()
        .and_then(|plist| product_version(&plist))
        .unwrap_or_default()
}

/// `ProductVersion` from a version file.
pub fn product_version(plist: &str) -> Option<String> {
    let after = plist.split_once("<key>ProductVersion</key>")?.1;
    let value = after.split_once("<string>")?.1.split_once("</string>")?.0;
    Some(value.trim().to_string())
}

/// Memory in whole gigabytes, rounded to the nearest.
pub fn memory_gb(total_bytes: u64) -> u64 {
    (total_bytes + (1 << 29)) >> 30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_and_memory_read_plainly() {
        let plist = "<dict>\n\t<key>ProductName</key>\n\t<string>macOS</string>\n\t<key>ProductVersion</key>\n\t<string>15.1</string>\n</dict>";
        assert_eq!(product_version(plist).as_deref(), Some("15.1"));
        assert_eq!(memory_gb(16 * (1 << 30) - 1000), 16);
        assert_eq!(memory_gb(8 << 30), 8);
    }
}
