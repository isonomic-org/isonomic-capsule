// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The harness host's `ModelClient` over the engine's model gateway, so an
//! agent session's turns run on the same local models as `models.stream`.
//!
//! The preset names a model as `local:default` (the user's default chat
//! model), `local:smallest` (the smallest installed one, for titles) or
//! `local:<id>`. The host runs as the engine, so the shell's access applies;
//! an app reaches the host only through its own sessions.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use gateway::Gateway;
use harness_core::future::BoxStream;
use harness_core::{ErrorInfo, Usage};
use harness_host::{CancelToken, ModelCaps, ModelClient};
use harness_log::{ModelEvent, ModelRequest, ModelStop};
use isonomic_inference_core::Capability;
use isonomic_model_library::ModelEntry;
use isonomic_proto::auth::Caller;
use tokio::sync::mpsc;

pub struct GatewayModel {
    gateway: Arc<Gateway>,
    /// How long a title waits after its turn before it is sent; `None`
    /// sends it at once and never yields (the hermetic tests' default).
    title_quiet: Option<Duration>,
    /// When the last reply was stopped, so the next one finds its slot.
    settle: Arc<crate::settle::Settle>,
    /// The gateway's count of the person's turns when the last reply
    /// began. A title that finds a later count yields at once: the person's
    /// next turn began before the title even started waiting.
    replied: Arc<AtomicU64>,
}

/// Whether `req` is the harness's title step. Its request carries no
/// purpose, and the model it names depends on the profile (the reply's own
/// model on the standard and high profiles), so a title is told apart by
/// the harness's own title instruction, which is its system prompt.
pub fn is_title(req: &ModelRequest) -> bool {
    req.system.trim() == harness_log::history::TITLE_PROMPT
}
/// The smallest catalogue model. It is tagged `tools` but calls them too
/// unreliably for display widgets, so it is offered none.
pub const NO_DISPLAY_TOOLS: &str = "qwen3.5-0.8b-q4-k-m";

/// Whether the harness may treat `entry` as a model that calls tools.
pub fn calls_tools(entry: &ModelEntry) -> bool {
    entry.meta.id.as_str() != NO_DISPLAY_TOOLS && entry.tags.iter().any(|t| t == "tools")
}

/// The engine's quiet period before a title: a quick follow-up never meets
/// one.
pub const TITLE_QUIET: Duration = Duration::from_secs(2);
/// The error a title that yielded ends with. The harness then gives the
/// chat its quick title from the first prompt and runs the person's turn.
pub const YIELDED: &str = "title.yielded";

impl GatewayModel {
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self {
            gateway,
            title_quiet: None,
            settle: Arc::default(),
            replied: Arc::default(),
        }
    }

    /// Titles yield to the person: each waits `quiet` after its turn, and
    /// a turn that begins meanwhile, or while the title is being written,
    /// ends it at once, its model request dropped so the server stops it.
    pub fn with_title_quiet(mut self, quiet: Duration) -> Self {
        self.title_quiet = Some(quiet);
        self
    }

    /// The catalogue id a preset's model name stands for.
    /// The catalogue id a model name stands for. An alias with no installed
    /// model answers `Ok(None)` (the gateway then says none is installed);
    /// a name the catalogue does not know, which a person may have picked,
    /// is an error rather than a silent fall back to another model.
    fn resolve(&self, model: &str) -> Result<Option<String>, String> {
        let catalogue = self.gateway.catalogue();
        let id = |e: &ModelEntry| e.meta.id.as_str().to_string();
        match model.trim().strip_prefix("local:").unwrap_or(model.trim()) {
            "default" => Ok(catalogue.default_for(Capability::Chat).map(id)),
            "smallest" => Ok(catalogue.smallest(Capability::Chat).map(id)),
            name => catalogue
                .get(name)
                .map(|e| Some(id(e)))
                .ok_or_else(|| name.to_string()),
        }
    }
}

/// Shortly after a stop, wait until the model server has ended the stopped
/// reply, so this one gets the conversation's slot; without a slot list,
/// wait about one token gap instead ([`crate::settle`]).
async fn after_stop(gateway: &Gateway, model: Option<&str>, settle: &crate::settle::Settle) {
    use gateway::slots::SlotWait;
    let Some(model) = model else { return };
    match gateway.settle_after_stop(model).await {
        None => {}
        Some(SlotWait::Idle(waited)) => capsule_core::log::line(&format!(
            "reply: the model server's slots were free after {} ms",
            waited.as_millis()
        )),
        Some(SlotWait::StillBusy) => capsule_core::log::line(&format!(
            "reply: a slot was still working after {} ms; sending anyway",
            gateway::slots::LIMIT.as_millis()
        )),
        Some(SlotWait::Unavailable(why)) => {
            capsule_core::log::line(&format!("reply: no slot list ({why}); waiting a token gap"));
            settle.settle().await;
        }
    }
}

fn yielded() -> ErrorInfo {
    ErrorInfo::new(YIELDED, "the title gave way to the person's next prompt")
}

fn error(message: impl Into<String>) -> ErrorInfo {
    ErrorInfo::new("model_unavailable", message)
}

impl ModelClient for GatewayModel {
    fn caps(&self, model: &str) -> ModelCaps {
        // What one conversation can hold: the model's own window, cut to
        // the chat worker's slot. Compaction works from this number.
        let entry = self
            .resolve(model)
            .ok()
            .flatten()
            .and_then(|id| self.gateway.catalogue().get(&id).cloned());
        let context = entry
            .as_ref()
            .and_then(|e| e.meta.context_length)
            .unwrap_or(gateway::SLOT_CONTEXT)
            .min(gateway::SLOT_CONTEXT);
        ModelCaps {
            // The llama.cpp server continues a final assistant message (its
            // assistant prefill), which is how the harness asks to continue.
            prefill: true,
            reasoning: true,
            // Tools reach the local model server, which calls them back.
            // The harness offers display tools only to a model that calls
            // tools: one the catalogue tags `tools`, except the smallest.
            tools: entry.as_ref().is_some_and(calls_tools),
            context_tokens: u64::from(context),
        }
    }

    fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelToken,
    ) -> BoxStream<'static, Result<ModelEvent, ErrorInfo>> {
        let gateway = Arc::clone(&self.gateway);
        let settle = Arc::clone(&self.settle);
        let (tx, rx) = mpsc::channel(64);
        let model = match self.resolve(&req.model) {
            Ok(model) => model,
            Err(name) => {
                // The turn fails with a reason a person can read.
                let message = format!("The model {name} is not available on this computer.");
                let _ = tx.try_send(Err(ErrorInfo::new("unknown_model", message)));
                return Box::pin(Events(rx));
            }
        };
        let model_id = model.clone();
        let request = crate::model_request::request(model, &req);
        let titling = is_title(&req);
        let quiet = self.title_quiet.filter(|_| titling);
        let replied = Arc::clone(&self.replied);
        tokio::spawn(async move {
            // A title waits for quiet, and yields to the person's next turn.
            let mut person = gateway.foreground();
            let turns = *person.borrow_and_update();
            if !titling {
                replied.store(turns, Ordering::SeqCst);
            } else if quiet.is_some() && turns != replied.load(Ordering::SeqCst) {
                // The person's next prompt came between the reply and this
                // title: `changed` below would never see it.
                capsule_core::log::line("title: yielded before it was sent");
                let _ = tx.send(Err(yielded())).await;
                return;
            }
            let mut stops = gateway.stops();
            stops.borrow_and_update();
            if let Some(quiet) = quiet {
                let waiting = format!("title: waiting {} ms for quiet", quiet.as_millis());
                capsule_core::log::line(&waiting);
                tokio::select! {
                    _ = tokio::time::sleep(quiet) => {}
                    _ = person.changed() => {
                        capsule_core::log::line("title: yielded before it was sent");
                        let _ = tx.send(Err(yielded())).await;
                        return;
                    }
                }
                if cancel.is_cancelled() {
                    return;
                }
                capsule_core::log::line("title: sent");
            } else {
                // A reply right after a stop waits for the stopped reply's
                // slot, which holds the conversation, to be freed.
                after_stop(&gateway, model_id.as_deref(), &settle).await;
            }
            let opened = tokio::select! {
                opened = gateway.chat(Caller::Shell, request) => opened,
                _ = person.changed(), if quiet.is_some() => {
                    capsule_core::log::line("title: yielded while the server opened it");
                    let _ = tx.send(Err(yielded())).await;
                    return;
                }
            };
            let mut stream = match opened {
                Ok((_, stream)) => stream,
                Err(e) => {
                    let _ = tx.send(Err(error(e.message))).await;
                    return;
                }
            };
            let mut stop = ModelStop::End;
            let mut calls = crate::tool_calls::ToolCalls::default();
            // The server streams about one token per chunk and reports no
            // usage through the provider, so chunks stand in for tokens.
            let mut chunks: u64 = 0;
            // How far apart the last two chunks came, for `settle`.
            let (mut last, mut gap): (Option<Instant>, Option<Duration>) = (None, None);
            loop {
                let item = tokio::select! {
                    item = stream.next() => item,
                    // The person stopped a reply: if it is this one, drop
                    // its request now, not at its next token.
                    _ = stops.changed() => {
                        if cancel.is_cancelled() {
                            settle.stopped(gap);
                            return;
                        }
                        continue;
                    }
                    // Dropping the stream drops the request: the model
                    // server stops writing the title.
                    _ = person.changed(), if quiet.is_some() => {
                        capsule_core::log::line("title: yielded while it was being written");
                        let _ = tx.send(Err(yielded())).await;
                        return;
                    }
                };
                let Some(item) = item else { break };
                let now = Instant::now();
                gap = last.map(|l| now - l).or(gap);
                last = Some(now);
                if cancel.is_cancelled() {
                    settle.stopped(gap);
                    return;
                }
                let chunk = match item {
                    Ok(chunk) => chunk,
                    Err(e) => {
                        let _ = tx.send(Err(error(e.to_string()))).await;
                        return;
                    }
                };
                let reasoned = chunk.reasoning.as_ref().is_some_and(|r| !r.is_empty());
                if !chunk.delta.is_empty() || reasoned {
                    chunks += 1;
                }
                if let Some(reasoning) = chunk.reasoning.filter(|r| !r.is_empty()) {
                    if tx.send(Ok(ModelEvent::Reasoning(reasoning))).await.is_err() {
                        return;
                    }
                }
                if !chunk.delta.is_empty()
                    && tx.send(Ok(ModelEvent::Text(chunk.delta))).await.is_err()
                {
                    return;
                }
                for delta in chunk.tool_calls {
                    calls.push(delta);
                }
                match chunk.finish_reason.as_deref() {
                    Some("length") => stop = ModelStop::MaxTokens,
                    Some("tool_calls") => stop = ModelStop::ToolUse,
                    _ => {}
                }
            }
            if !calls.is_empty() {
                stop = ModelStop::ToolUse;
            }
            for (tool, input) in calls.finish() {
                if tx
                    .send(Ok(ModelEvent::ToolCall { tool, input }))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let usage = Usage {
                output_tokens: chunks,
                ..Usage::default()
            };
            let _ = tx.send(Ok(ModelEvent::Done { stop, usage })).await;
        });
        Box::pin(Events(rx))
    }
}

/// The forwarding task's output, as the harness's own stream type.
struct Events(mpsc::Receiver<Result<ModelEvent, ErrorInfo>>);

impl harness_core::future::Stream for Events {
    type Item = Result<ModelEvent, ErrorInfo>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.get_mut().0.poll_recv(cx)
    }
}

#[cfg(test)]
mod caps_tests {
    use super::*;
    use std::sync::Arc;

    struct Open;
    impl capsule_core::RamGate for Open {
        fn admit(&self, _: u64) -> Result<(), capsule_core::RamDenial> {
            Ok(())
        }
    }

    fn model(context: u32) -> (tempfile::TempDir, GatewayModel) {
        let hash = "0".repeat(64);
        let entry = |id: &str, ctx: u32| {
            format!(
                r#"{{"id":"{id}","name":"{id}","file_name":"{id}.gguf","source_url":"https://example.invalid/{id}","sha256":"{hash}","size_bytes":1,"context_length":{ctx},"capabilities":["chat"],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}"#
            )
        };
        let json = format!(
            r#"{{"default_chat":"big","models":[{},{}]}}"#,
            entry("big", 262144),
            entry("small", context)
        );
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(dir.join("big.gguf"), [0]).unwrap();
        std::fs::write(dir.join("small.gguf"), [0]).unwrap();
        let manifest = isonomic_model_library::Manifest::from_json(&json).unwrap();
        let gateway = gateway::Gateway::new(manifest, dir, None, Arc::new(Open), false);
        (tmp, GatewayModel::new(Arc::new(gateway)))
    }

    #[test]
    fn a_models_context_is_cut_to_the_workers_slot() {
        let (_dir, m) = model(2048);
        assert_eq!(m.caps("local:default").context_tokens, 8192);
        assert_eq!(m.caps("big").context_tokens, 8192);
        assert_eq!(m.caps("local:small").context_tokens, 2048);
        assert_eq!(m.caps("nope").context_tokens, 8192);
    }

    #[test]
    fn display_tools_go_only_to_tagged_models_but_the_smallest() {
        use isonomic_inference_core::ModelRef;
        let hash = "0".repeat(64);
        let entry = |id: &str, tags: &str| {
            format!(
                r#"{{"id":"{id}","name":"{id}","file_name":"{id}.gguf","source_url":"https://example.invalid/{id}","sha256":"{hash}","size_bytes":1,"capabilities":["chat"],"tags":[{tags}],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}"#
            )
        };
        let json = format!(
            r#"{{"default_chat":"tagged","models":[{},{},{}]}}"#,
            entry("tagged", r#""tools","vision""#),
            entry("plain", r#""vision""#),
            entry(NO_DISPLAY_TOOLS, r#""tools""#)
        );
        let manifest = isonomic_model_library::Manifest::from_json(&json).unwrap();
        let calls = |id: &str| calls_tools(manifest.get(&ModelRef::new(id)).unwrap());
        assert!(calls("tagged"));
        assert!(!calls("plain"));
        assert!(!calls(NO_DISPLAY_TOOLS));
    }
}
