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
use isonomic_model_library::ModelEntry;
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

/// A message's words. Request context (the date, the language) is left
/// out: it goes to the system message, see [`request`].
fn text(parts: &[Part]) -> String {
    let mut out = String::new();
    for part in parts {
        let piece = match part {
            Part::Text { text } | Part::Summary { text } => text,
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
///
/// The request context the harness puts in a user turn ("Current date: …")
/// moves to the end of the system message, latest only. Small local models
/// read it there as background; glued to the person's words, it made them
/// ask which day was meant instead of answering.
fn request(model: Option<String>, req: &ModelRequest) -> StreamRequest {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    let context = req.messages.iter().rev().find_map(|m| {
        m.content.iter().rev().find_map(|p| match p {
            Part::Context { text } => Some(text.clone()),
            _ => None,
        })
    });
    let system = match (req.system.is_empty(), context) {
        (true, None) => None,
        (false, None) => Some(req.system.clone()),
        (true, Some(context)) => Some(context),
        (false, Some(context)) => Some(format!("{}\n\n{context}", req.system)),
    };
    if let Some(system) = system {
        messages.push(Message::system(system));
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
            .ok()
            .flatten()
            .and_then(|id| self.gateway.catalogue().get(&id).cloned())
            .and_then(|e| e.meta.context_length)
            .unwrap_or(4096);
        ModelCaps {
            // The llama.cpp server continues a final assistant message (its
            // assistant prefill), which is how the harness asks to continue.
            prefill: true,
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
        let request = request(model, &req);
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
            // The server streams about one token per chunk and reports no
            // usage through the provider, so chunks stand in for tokens.
            let mut chunks: u64 = 0;
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
                if chunk.finish_reason.as_deref() == Some("length") {
                    stop = ModelStop::MaxTokens;
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
mod tests {
    use super::*;

    #[test]
    fn a_continue_request_ends_with_the_partial_reply() {
        let req: ModelRequest = serde_json::from_value(serde_json::json!({
            "model": "local:default",
            "params": {"max_output_tokens": 256, "reasoning": "off"},
            "system": "Be brief.",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Count to five"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "One, two,"}]}
            ],
            "prefill": true
        }))
        .unwrap();
        let out = request(Some("chat-small".into()), &req);
        let roles: Vec<_> = out.messages.iter().map(|m| m.role).collect();
        assert_eq!(out.messages[0].content, "Be brief.");
        assert_eq!(roles, [Role::System, Role::User, Role::Assistant]);
        assert_eq!(out.messages[2].content, "One, two,");
        assert_eq!(out.max_tokens, Some(256));
        assert_eq!(out.reasoning, Some(false));
    }

    #[test]
    fn the_request_context_moves_from_the_user_turn_to_the_system_message() {
        let req: ModelRequest = serde_json::from_value(serde_json::json!({
            "model": "local:default",
            "params": {"max_output_tokens": 64, "reasoning": "off"},
            "system": "Be brief.",
            "messages": [{"role": "user", "content": [
                {"type": "context", "text": "Current date: Sunday, 15 November 2026. Language: en."},
                {"type": "text", "text": "Plan one day in Lisbon."}
            ]}]
        }))
        .unwrap();
        let out = request(None, &req);
        assert_eq!(
            out.messages[0].content,
            "Be brief.\n\nCurrent date: Sunday, 15 November 2026. Language: en."
        );
        assert_eq!(out.messages[1].content, "Plan one day in Lisbon.");
    }
}
