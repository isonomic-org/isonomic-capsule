//! Bundled binaries (architecture spec, section 11).
//!
//! Everything the engine runs ships inside the app, pinned and verified:
//!
//! - `manifest`: the pinned downloads and the rules a manifest must satisfy.
//! - `install`: download, verify the SHA-256, unpack, and check the
//!   architecture of every artifact, including ones already installed.
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

pub use bundle::{host_triple, BinaryStatus, Bundle};
pub use install::{install_one, Downloader, InstallError, Outcome};
pub use manifest::{Manifest, ManifestError, Sidecar, Target, APPLE_SLICES, UNIVERSAL_TRIPLE};
