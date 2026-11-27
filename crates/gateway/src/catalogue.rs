// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which models exist and which are installed: the model library's
//! manifest, and the files in the Capsule's model directory.

use std::path::{Path, PathBuf};

use isonomic_inference_core::{Capability, ModelRef};
use isonomic_model_library::{Manifest, ModelEntry};
use isonomic_proto::models::{ModelDetail, ModelInfo, ModelState};

/// The chat model for slow machines (Intel Macs, the low profile): the
/// smallest that answers usably. The 0.8B model is faster but invents
/// facts; it stays for titles (`local:smallest`).
pub const SLOW_CHAT_MODEL: &str = "qwen3.5-2b-q4-k-m";

pub struct Catalogue {
    manifest: Manifest,
    dir: PathBuf,
    /// A machine where the manifest's default chat model is too slow to
    /// talk to: pick the smallest installed one instead.
    slow: bool,
}

impl Catalogue {
    pub fn new(manifest: Manifest, dir: PathBuf, slow: bool) -> Self {
        Self {
            manifest,
            dir,
            slow,
        }
    }

    /// Whether this is a slow machine: models run on the processor alone,
    /// or the profile is low.
    pub fn slow(&self) -> bool {
        self.slow
    }

    /// Every model of the manifest, installed or not.
    pub fn entries(&self) -> &[ModelEntry] {
        self.manifest.entries()
    }

    /// The model directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The model to recommend first: [`SLOW_CHAT_MODEL`] on a slow machine
    /// or the low profile (the smallest chat model when the manifest has
    /// no such entry), else the manifest's default. Installed or not.
    pub fn first_model(&self, small: bool) -> &ModelEntry {
        if !(small || self.slow) {
            return self.manifest.default_chat();
        }
        let smallest = self
            .manifest
            .entries()
            .iter()
            .filter(|e| e.meta.supports(Capability::Chat))
            .min_by_key(|e| e.meta.size_bytes);
        self.get(SLOW_CHAT_MODEL)
            .or(smallest)
            .unwrap_or_else(|| self.manifest.default_chat())
    }

    /// Where a model's file is, installed or not.
    pub fn path(&self, entry: &ModelEntry) -> PathBuf {
        self.dir.join(&entry.file_name)
    }

    /// Installed: its file is there at its full size.
    pub fn installed(&self, entry: &ModelEntry) -> bool {
        file_size(&self.path(entry)) == Some(entry.meta.size_bytes)
    }

    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.manifest.get(&ModelRef::new(id))
    }

    /// Every installed model, in the manifest's order.
    pub fn installed_entries(&self) -> impl Iterator<Item = &ModelEntry> {
        self.manifest.entries().iter().filter(|e| self.installed(e))
    }

    /// The smallest installed model that can do `capability`.
    pub fn smallest(&self, capability: Capability) -> Option<&ModelEntry> {
        self.installed_entries()
            .filter(|e| e.meta.supports(capability))
            .min_by_key(|e| e.meta.size_bytes)
    }

    /// The user's default model for `capability`, among the installed ones.
    pub fn default_for(&self, capability: Capability) -> Option<&ModelEntry> {
        let preferred = match capability {
            Capability::Chat => Some(self.manifest.default_chat()),
            _ => self.manifest.default_embeddings(),
        };
        let candidates = || {
            self.installed_entries()
                .filter(move |e| e.meta.supports(capability))
        };
        if capability == Capability::Chat && self.slow {
            let slow = self.get(SLOW_CHAT_MODEL).filter(|e| self.installed(e));
            return slow.or_else(|| self.smallest(capability));
        }
        match preferred {
            Some(p) if self.installed(p) => Some(p),
            _ => candidates().next(),
        }
    }
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// A model as listed. `recommended` marks the one `models.recommended`
/// names for this machine, for the model picker.
pub fn info(entry: &ModelEntry, recommended: bool) -> ModelInfo {
    let meta = &entry.meta;
    ModelInfo {
        id: meta.id.as_str().to_string(),
        name: meta.name.clone(),
        context_length: meta.context_length,
        capabilities: meta
            .capabilities
            .iter()
            .map(|c| c.as_str().to_string())
            .collect(),
        remote: false,
        recommended,
    }
}

pub fn detail(entry: &ModelEntry, state: ModelState, recommended: bool) -> ModelDetail {
    ModelDetail {
        info: info(entry, recommended),
        size_bytes: Some(entry.meta.size_bytes),
        license: Some(entry.meta.license.name.clone()),
        state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Install" a model of the built-in manifest: a sparse file of its size.
    fn install(catalogue: &Catalogue, id: &str) {
        let entry = catalogue.get(id).expect(id);
        let file = std::fs::File::create(catalogue.path(entry)).unwrap();
        file.set_len(entry.meta.size_bytes).unwrap();
    }

    fn catalogue(dir: &Path, slow: bool) -> Catalogue {
        Catalogue::new(Manifest::builtin(), dir.to_path_buf(), slow)
    }

    #[test]
    fn slow_machines_default_to_the_two_billion_model_when_installed() {
        let dir = tempfile::tempdir().unwrap();
        let slow = catalogue(dir.path(), true);
        install(&slow, "qwen3.5-0.8b-q4-k-m");
        let id = |e: Option<&ModelEntry>| e.map(|e| e.meta.id.as_str().to_string());
        // Only the 0.8B model: it is what there is.
        assert_eq!(
            id(slow.default_for(Capability::Chat)).as_deref(),
            Some("qwen3.5-0.8b-q4-k-m")
        );
        install(&slow, SLOW_CHAT_MODEL);
        assert_eq!(
            id(slow.default_for(Capability::Chat)).as_deref(),
            Some(SLOW_CHAT_MODEL)
        );
        // Titles still go to the smallest.
        assert_eq!(
            id(slow.smallest(Capability::Chat)).as_deref(),
            Some("qwen3.5-0.8b-q4-k-m")
        );
    }

    #[test]
    fn the_first_model_is_the_two_billion_one_on_slow_machines_only() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            catalogue(dir.path(), true)
                .first_model(false)
                .meta
                .id
                .as_str(),
            SLOW_CHAT_MODEL
        );
        assert_eq!(
            catalogue(dir.path(), false)
                .first_model(true)
                .meta
                .id
                .as_str(),
            SLOW_CHAT_MODEL
        );
        let fast = catalogue(dir.path(), false);
        let default = Manifest::builtin()
            .default_chat()
            .meta
            .id
            .as_str()
            .to_string();
        assert_eq!(fast.first_model(false).meta.id.as_str(), default);
    }
}
