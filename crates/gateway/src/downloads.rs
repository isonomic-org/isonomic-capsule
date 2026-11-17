// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Model downloads (isonomic-sdk #26): the engine owns each one, so any
//! window can follow it, leave, and attach again.
//!
//! A download runs in the engine (`crate::fetch`): the file arrives as
//! `<name>.part`, continues from there after a quit, sleep or lost
//! connection, and is renamed into the model directory only once its
//! checksum matches, so a model is only ever installed whole. The
//! latest progress sits in a watch channel. A follower is sent where the
//! download stands at once, then every change, and the event with an
//! `outcome` is the last. Cancel takes the model, since several windows may
//! follow one download.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use broker::agents::Starter;
use isonomic_model_library::ModelEntry;
use isonomic_proto::models::{DownloadOutcome, DownloadProgress};
use isonomic_proto::{ErrorCode, Event, ServerMessage, WireError};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

struct Running {
    progress: watch::Sender<DownloadProgress>,
    task: JoinHandle<()>,
}

#[derive(Default)]
pub struct Downloads {
    running: Arc<Mutex<HashMap<String, Running>>>,
    client: reqwest::Client,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn progress(model: &str, received: u64, total: Option<u64>) -> DownloadProgress {
    DownloadProgress {
        model: model.to_string(),
        received_bytes: received,
        // Absent when unknown, never 0.
        total_bytes: total.filter(|t| *t > 0),
        outcome: None,
        error: None,
    }
}

fn finished(mut p: DownloadProgress, outcome: DownloadOutcome) -> DownloadProgress {
    p.outcome = Some(outcome);
    p
}

/// Send `first`, then each change of `watch`, until one has an outcome.
fn follow(
    first: Option<DownloadProgress>,
    rx: Option<watch::Receiver<DownloadProgress>>,
) -> Starter {
    Box::new(
        move |subscription: u64, outbox: mpsc::Sender<ServerMessage>| {
            tokio::spawn(async move {
                let send = |p: DownloadProgress| ServerMessage::Event {
                    subscription,
                    event: Event::DownloadProgress(p),
                };
                if let Some(first) = first {
                    let last = first.outcome.is_some();
                    if outbox.send(send(first)).await.is_err() || last {
                        return;
                    }
                }
                let Some(mut rx) = rx else { return };
                while rx.changed().await.is_ok() {
                    let now = rx.borrow_and_update().clone();
                    let last = now.outcome.is_some();
                    if outbox.send(send(now)).await.is_err() || last {
                        return;
                    }
                }
            })
        },
    )
}

impl Downloads {
    /// Where `model`'s download stands, if one is running.
    pub fn received(&self, model: &str) -> Option<DownloadProgress> {
        lock(&self.running)
            .get(model)
            .map(|r| r.progress.borrow().clone())
    }

    /// `models.download` for an entry not yet installed: attach to the
    /// running download, or start one into `dir`.
    pub fn start(
        &self,
        entry: &ModelEntry,
        spec: isonomic_model_library::ModelSpec,
        dir: std::path::PathBuf,
        stall: crate::fetch::Stall,
    ) -> (Option<u64>, Starter) {
        let model = entry.meta.id.as_str().to_string();
        let known = Some(entry.meta.size_bytes).filter(|s| *s > 0);
        let mut running = lock(&self.running);
        if let Some(r) = running.get(&model) {
            let now = r.progress.borrow().clone();
            return (
                now.total_bytes,
                follow(Some(now), Some(r.progress.subscribe())),
            );
        }
        let already = crate::fetch::part_len(&dir, &spec);
        let (tx, rx) = watch::channel(progress(&model, already, known));
        let client = self.client.clone();
        let all = Arc::clone(&self.running);
        let updates = tx.clone();
        let id = model.clone();
        let task = tokio::spawn(async move {
            let mut total = known;
            let mut report = |bytes: u64, of: Option<u64>| {
                total = of.or(total);
                updates.send_replace(progress(&id, bytes, total));
            };
            let result = crate::fetch::fetch(&client, &spec, &dir, &stall, &mut report).await;
            let received = updates.borrow().received_bytes;
            let last = match result {
                Ok(_) => finished(
                    progress(&id, total.unwrap_or(received), total),
                    DownloadOutcome::Installed,
                ),
                Err(e) => {
                    let mut p = finished(progress(&id, received, total), DownloadOutcome::Failed);
                    p.error = Some(WireError::new(ErrorCode::ModelUnavailable, e.to_string()));
                    p
                }
            };
            // Finished: no longer running, then tell every follower.
            lock(&all).remove(&id);
            updates.send_replace(last);
        });
        running.insert(model, Running { progress: tx, task });
        (known, follow(None, Some(rx)))
    }

    /// `models.download_cancel`: stop it and end every follower cancelled.
    pub fn cancel(&self, model: &str) -> Result<(), WireError> {
        let Some(r) = lock(&self.running).remove(model) else {
            let message = format!("{model} is not downloading");
            return Err(WireError::new(ErrorCode::NotDownloading, message));
        };
        r.task.abort();
        let now = r.progress.borrow().clone();
        r.progress
            .send_replace(finished(now, DownloadOutcome::Cancelled));
        Ok(())
    }
}

/// The single event an installed model's `models.download` gets.
pub fn already_installed(entry: &ModelEntry) -> (Option<u64>, Starter) {
    let size = Some(entry.meta.size_bytes).filter(|s| *s > 0);
    let id = entry.meta.id.as_str();
    let done = finished(
        progress(id, entry.meta.size_bytes, size),
        DownloadOutcome::Installed,
    );
    (size, follow(Some(done), None))
}
