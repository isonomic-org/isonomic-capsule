//! The notice the app ships for the programs it redistributes.
//!
//! The database, the cache and the model server are bundled in the app
//! (the database and the cache as our own builds), and each one's licence
//! asks that its copyright notice and licence text come with every copy.
//! Each install keeps the licence text from its own archive or source tree
//! (`install::LICENSE`); this gathers them into one file beside the
//! installs, `NOTICES.txt`, which the app bundle carries.

use std::path::{Path, PathBuf};

use crate::install::{destination, LICENSE};
use crate::manifest::Manifest;

/// The notice's file name, in the manifest's output directory.
pub const NOTICE_FILE: &str = "NOTICES.txt";

const OPENING: &str = "Isonomic includes the following programs, each under its own \
licence. Each licence asks that its copyright notice and licence text come with every \
copy, so they are reproduced here in full.\n";

/// The notice for every installed program the manifest bundles, and the
/// names of bundled programs with no licence text installed yet.
pub fn notice(root: &Path, manifest: &Manifest) -> (String, Vec<String>) {
    let mut text = String::from(OPENING);
    let mut missing = Vec::new();
    for sidecar in &manifest.sidecars {
        if sidecar.is_deferred() || sidecar.is_draft() || sidecar.license_files.is_none() {
            continue;
        }
        let found = sidecar.targets.keys().find_map(|triple| {
            let dest = destination(root, manifest, &sidecar.name, triple);
            let licence = std::fs::read_to_string(dest.join(LICENSE)).ok()?;
            let url = sidecar.target(triple).map(|t| t.url).unwrap_or_default();
            Some((licence, url))
        });
        let Some((licence, url)) = found else {
            missing.push(sidecar.name.clone());
            continue;
        };
        text.push_str(&format!(
            "\n{rule}\n{} {} ({})\nFrom {url}\n{rule}\n\n{}\n",
            sidecar.name,
            sidecar.version,
            sidecar.license,
            licence.trim_end(),
            rule = "=".repeat(72),
        ));
    }
    (text, missing)
}

/// Write the notice beside the installs; where it went, and what is missing.
pub fn write_notice(root: &Path, manifest: &Manifest) -> std::io::Result<(PathBuf, Vec<String>)> {
    let (text, missing) = notice(root, manifest);
    let path = root.join(&manifest.out_dir).join(NOTICE_FILE);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, text)?;
    Ok((path, missing))
}
