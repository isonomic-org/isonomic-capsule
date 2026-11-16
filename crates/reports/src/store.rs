//! Writing a report into the Capsule's `reports/pending/`, beside the
//! window's and named as the window names its own
//! (`<unix millis, 16 digits>-<kind>.json`), so the window lists, shows and
//! prunes them with its own. Each is written to a `.part` file and renamed,
//! so the window never reads half a report. Nothing here sends.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// At most this many reports are kept; the oldest go first. The window's
/// own limit.
pub const KEEP_MOST: usize = 20;

pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store inside the Capsule at `capsule`.
    pub fn in_capsule(capsule: &Path) -> Self {
        Self {
            dir: capsule.join("reports").join("pending"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write `document` as a new report.
    pub fn save(&self, document: &Value, unix_millis: u128) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        let kind = document["kind"].as_str().unwrap_or("report");
        let mut path = self.dir.join(format!("{unix_millis:016}-{kind}.json"));
        let mut n = 1;
        while path.exists() {
            path = self.dir.join(format!("{unix_millis:016}-{kind}-{n}.json"));
            n += 1;
        }
        let bytes = serde_json::to_string_pretty(document).map_err(std::io::Error::other)?;
        let part = path.with_extension("json.part");
        std::fs::write(&part, bytes)?;
        std::fs::rename(&part, &path)?;
        self.keep_most();
        Ok(path)
    }

    /// Every kept report's path, oldest first.
    pub fn pending(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        paths
    }

    /// A service failing in a loop must not fill the folder.
    fn keep_most(&self) {
        let pending = self.pending();
        let excess = pending.len().saturating_sub(KEEP_MOST);
        for path in pending.iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reports_are_named_as_the_windows_and_kept_to_a_limit() {
        let capsule = tempfile::tempdir().unwrap();
        let store = Store::in_capsule(capsule.path());
        let first = store
            .save(&json!({"kind": "service_failed"}), 1_000)
            .unwrap();
        assert_eq!(
            first.file_name().unwrap(),
            "0000000000001000-service_failed.json"
        );
        let again = store
            .save(&json!({"kind": "service_failed"}), 1_000)
            .unwrap();
        assert_eq!(
            again.file_name().unwrap(),
            "0000000000001000-service_failed-1.json"
        );
        for i in 0..KEEP_MOST {
            store
                .save(&json!({"kind": "panic"}), 2_000 + i as u128)
                .unwrap();
        }
        let pending = store.pending();
        assert_eq!(pending.len(), KEEP_MOST);
        assert!(!pending.contains(&first));
        let parts = std::fs::read_dir(store.dir())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "part"))
            .count();
        assert_eq!(parts, 0);
    }
}
