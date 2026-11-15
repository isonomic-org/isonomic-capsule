//! The harness host's `ModelClient` over the engine's model gateway, so an
//! agent session's turns run on the same local models as `models.stream`.
//!
//! The preset names a model as `local:default` (the user's default chat
//! model), `local:smallest` (the smallest installed one, for titles) or
//! `local:<id>`. The host runs as the engine, so the shell's access applies;
//! an app reaches the host only through its own sessions.

use std::sync::Arc;

use futures_util::StreamExt;
use gateway::Gateway;
use harness_core::future::BoxStream;
use harness_core::{ErrorInfo, Usage};
use harness_host::{CancelToken, ModelCaps, ModelClient};
use harness_log::{ModelEvent, ModelRequest, ModelStop, Part, Role as LogRole};
use isonomic_inference_core::Capability;
use isonomic_proto::auth::Caller;
use isonomic_proto::models::{Message, Role, StreamRequest};
use tokio::sync::mpsc;

pub struct GatewayModel {
    gateway: Arc<Gateway>,
}

impl GatewayModel {
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self { gateway }
    }

    /// The catalogue id a preset's model name stands for.
    fn resolve(&self, model: &str) -> Option<String> {
        let catalogue = self.gateway.catalogue();
        let entry = match model.strip_prefix("local:").unwrap_or(model) {
            "default" => catalogue.default_for(Capability::Chat),
            "smallest" => catalogue.smallest(Capability::Chat),
            id => catalogue.get(id),
        };
        entry.map(|e| e.meta.id.as_str().to_string())
    }
}

fn text(parts: &[Part]) -> String {
    let mut out = String::new();
    for part in parts {
        let piece = match part {
            Part::Text { text } | Part::Context { text } | Part::Summary { text } => text,
            Part::Attachment { name, text, .. } => {
                out.push_str(&format!("[{name}]\n"));
                text
            }
            Part::Mention { label, .. } => label,
            // Tool calls and results: V1 sessions use none.
            _ => continue,
        };
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(piece);
    }
    out
}

/// The harness's request as the gateway takes it.
fn request(model: Option<String>, req: &ModelRequest) -> StreamRequest {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if !req.system.is_empty() {
        messages.push(Message::system(req.system.clone()));
    }
    for m in &req.messages {
        let role = match m.role {
            LogRole::User => Role::User,
            LogRole::Assistant => Role::Assistant,
            LogRole::Tool => Role::Tool,
        };
        messages.push(Message {
            role,
            content: text(&m.content),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    let reasoning = serde_json::to_value(&req.params.reasoning).ok();
    let off = reasoning.as_ref().and_then(|v| v.as_str()) == Some("off");
    StreamRequest {
        model,
        messages,
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(req.params.max_output_tokens).filter(|n| *n > 0),
        // Off when the preset says so; otherwise the engine decides.
        reasoning: off.then_some(false),
    }
}

fn error(message: impl Into<String>) -> ErrorInfo {
    ErrorInfo::new("model_unavailable", message)
}

impl ModelClient for GatewayModel {
    fn caps(&self, model: &str) -> ModelCaps {
        let context = self
            .resolve(model)
            .and_then(|id| self.gateway.catalogue().get(&id).cloned())
            .and_then(|e| e.meta.context_length)
            .unwrap_or(4096);
        ModelCaps {
            // The local server does not continue an assistant prefix.
            prefill: false,
            reasoning: true,
            tools: false,
            context_tokens: u64::from(context),
        }
    }

    fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelToken,
    ) -> BoxStream<'static, Result<ModelEvent, ErrorInfo>> {
        let gateway = Arc::clone(&self.gateway);
        let request = request(self.resolve(&req.model), &req);
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let opened = gateway.chat(Caller::Shell, request).await;
            let mut stream = match opened {
                Ok((_, stream)) => stream,
                Err(e) => {
                    let _ = tx.send(Err(error(e.message))).await;
                    return;
                }
            };
            let mut stop = ModelStop::End;
            while let Some(item) = stream.next().await {
                if cancel.is_cancelled() {
                    return;
                }
                let chunk = match item {
                    Ok(chunk) => chunk,
                    Err(e) => {
                        let _ = tx.send(Err(error(e.to_string()))).await;
                        return;
                    }
                };
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
                if chunk.finish_reason.as_deref() == Some("length") {
                    stop = ModelStop::MaxTokens;
                }
            }
            let usage = Usage::default();
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
