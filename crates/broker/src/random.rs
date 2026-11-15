//! Bytes from the system's random source, for secrets and opaque handles.

use std::io::Read;

/// `N` random bytes.
pub(crate) fn bytes<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut out = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut out)?;
    Ok(out)
}

/// Lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
