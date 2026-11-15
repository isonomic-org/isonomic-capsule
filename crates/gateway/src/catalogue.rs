//! Which models exist and which are installed: the model library's
//! manifest, and the files in the Capsule's model directory.

use std::path::{Path, PathBuf};

use isonomic_inference_core::{Capability, ModelRef};
use isonomic_model_library::{Manifest, ModelEntry};
use isonomic_proto::models::{ModelDetail, ModelInfo, ModelState};

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
            return self.smallest(capability);
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

pub fn info(entry: &ModelEntry) -> ModelInfo {
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
    }
}

pub fn detail(entry: &ModelEntry, state: ModelState) -> ModelDetail {
    ModelDetail {
        info: info(entry),
        size_bytes: Some(entry.meta.size_bytes),
        license: Some(entry.meta.license.name.clone()),
        state,
    }
}
