// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The model setup: `models.catalogue` lists every local model with a
//! label for this machine, and `models.remove` uninstalls one.

use isonomic_inference_core::Capability;
use isonomic_model_library::ModelEntry;
use isonomic_proto::models::{CatalogueEntry, ModelState};
use isonomic_proto::{ErrorCode, WireError};

use crate::fit::{fit, MachineFit};
use crate::Gateway;

impl Gateway {
    /// What the fit rules read about this machine.
    fn machine_fit(&self) -> MachineFit {
        let slow_limit = self
            .catalogue()
            .get(crate::SLOW_CHAT_MODEL)
            .map(|e| e.meta.size_bytes)
            .unwrap_or(3 << 30);
        MachineFit::new(&self.machine, self.catalogue().slow(), slow_limit)
    }

    /// `models.catalogue`: every chat and embedding model in the manifest,
    /// installed or not, each with its state and its label for this
    /// machine. The manifest is the only source of local models, so an
    /// installed model is always one of its entries.
    pub fn catalogue_entries(&self) -> Vec<CatalogueEntry> {
        let machine = self.machine_fit();
        let recommended = self.recommended_id();
        self.catalogue()
            .entries()
            .iter()
            .filter(|e| {
                e.meta.supports(Capability::Chat) || e.meta.supports(Capability::Embeddings)
            })
            .map(|e| {
                let id = e.meta.id.as_str();
                let chat = e.meta.supports(Capability::Chat);
                let (fits, reason) = fit(&machine, e.meta.size_bytes, chat, e.min_ram_gb);
                let marked = recommended.as_deref() == Some(id);
                CatalogueEntry {
                    detail: crate::catalogue::detail(e, self.state(e), marked),
                    about: e.about.clone(),
                    publisher: e.publisher.clone(),
                    fits,
                    fit_reason: Some(reason),
                }
            })
            .collect()
    }

    /// `models.remove`: delete an installed model's file. A model whose
    /// worker runs is unloaded first; a chat that needs it again gets
    /// `model_unavailable` until it is downloaded again.
    pub async fn remove(&self, id: &str) -> Result<(), WireError> {
        let entry: &ModelEntry = self
            .catalogue()
            .get(id)
            .ok_or_else(|| WireError::new(ErrorCode::UnknownModel, format!("no model {id}")))?;
        if self.state(entry) == ModelState::Downloading {
            let message = format!("{id} is downloading; cancel the download first");
            return Err(WireError::new(ErrorCode::DownloadRunning, message));
        }
        if !self.catalogue().installed(entry) {
            let message = format!("{id} is not installed");
            return Err(WireError::new(ErrorCode::NotInstalled, message));
        }
        let worker = self
            .workers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
        if let Some(worker) = worker {
            worker.supervisor().stop().await;
        }
        let path = self.catalogue().path(entry);
        std::fs::remove_file(&path).map_err(|e| {
            let message = format!("could not delete the file of {id}: {e}");
            WireError::new(ErrorCode::Internal, message)
        })?;
        capsule_core::log::line(&format!("models: removed {id}"));
        Ok(())
    }
}
