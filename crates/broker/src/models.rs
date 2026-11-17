// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The seam to the model gateway: the broker checks the token and the
//! capability, then hands every `models` call here. The gateway lives in
//! its own crate, which depends on this one.

use isonomic_proto::{Call, ErrorCode, Manifest, Reply, ServerMessage, WireError};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::agents::BoxFuture;

/// Starts a model stream's forwarding task, given its subscription id, the
/// outbox, and a signal that fires on `models.cancel`. The task ends the
/// stream with exactly one chunk that has `stop`.
pub type StreamStarter = Box<
    dyn FnOnce(u64, mpsc::Sender<ServerMessage>, oneshot::Receiver<()>) -> JoinHandle<()> + Send,
>;

pub enum ModelsAnswer {
    Reply(Reply),
    /// `models.stream`: reply `stream { subscription, model }`, then start.
    Stream {
        model: String,
        start: StreamStarter,
    },
    /// `models.download`: reply `downloading { subscription, model,
    /// total_bytes }`, then forward `download_progress` events.
    Download {
        model: String,
        total_bytes: Option<u64>,
        start: crate::agents::Starter,
    },
}

pub trait Models: Send + Sync + 'static {
    /// Answer one models call for `manifest`'s app (`None` is the shell).
    /// `proto::auth::authorize` has already run; `models.cancel` is the
    /// broker's own.
    fn answer<'a>(
        &'a self,
        manifest: Option<&'a Manifest>,
        call: Call,
    ) -> BoxFuture<'a, Result<ModelsAnswer, WireError>>;
}

/// No gateway: every call is answered `unknown_method`.
pub struct NoModels;

impl Models for NoModels {
    fn answer<'a>(
        &'a self,
        _: Option<&'a Manifest>,
        call: Call,
    ) -> BoxFuture<'a, Result<ModelsAnswer, WireError>> {
        let method = call.method();
        Box::pin(async move {
            Err(WireError::new(
                ErrorCode::UnknownMethod,
                format!("this engine does not serve {method} yet"),
            ))
        })
    }
}
