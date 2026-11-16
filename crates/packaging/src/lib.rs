//! Bundled binaries (architecture spec, section 11).
//!
//! Everything the engine runs ships inside the app, pinned and verified:
//!
//! - `manifest`: the pinned downloads and the rules a manifest must satisfy.
//! - `install`: download, verify the SHA-256, and check the architecture of
//!   every artifact, including ones already installed.
//! - `place`: put a verified download in place: a directory tree, one
//!   program, or one program built from verified source.
//! - `macho`: read which architectures a Mach-O file contains.
//! - `bundle`: find a bundled binary at run time.
//!
//! Binaries are never committed. A download that does not match its pin, a
//! pin that is missing, and a binary built for the wrong architecture all
//! stop the build: nothing fails open.

pub mod bundle;
pub mod install;
pub mod macho;
pub mod manifest;
#[cfg(test)]
mod minimum_tests;
pub mod minos;
pub mod notices;
pub mod place;

pub use bundle::{host_triple, BinaryStatus, Bundle};
pub use install::{install_one, Downloader, InstallError, Outcome};
pub use manifest::{Manifest, ManifestError, Sidecar, Target, APPLE_SLICES, UNIVERSAL_TRIPLE};
