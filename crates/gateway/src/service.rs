//! The `models` methods over the gateway, and a stream's chunks on the
//! wire: text and reasoning as they come, then exactly one chunk with
//! `stop`, whether the model ended, failed or was cancelled.

use broker::agents::BoxFuture;
use broker::models::{ModelsAnswer, StreamStarter};
use futures_util::StreamExt;
use isonomic_proto::auth::Caller;
use isonomic_proto::models::{ModelChunk, Recommendation, StopReason};
use isonomic_proto::{
    Call, ErrorCode, Event, Manifest, MemoryRefusal, Reply, ServerMessage, WireError,
};
use isonomic_provider::ChatStream;
use tokio::sync::{mpsc, oneshot};

use crate::catalogue::{detail, info};
use crate::{failure, Gateway};

fn caller(manifest: Option<&Manifest>) -> Caller<'_> {
    match manifest {
        None => Caller::Shell,
        Some(m) => Caller::App {
            id: &m.id,
            capabilities: &m.capabilities,
        },
    }
}

/// The wire's stop for the server's finish reason.
fn stop(reason: &str) -> StopReason {
    match reason {
        "length" => StopReason::MaxTokens,
        "tool_calls" => StopReason::ToolCalls,
        _ => StopReason::End,
    }
}

fn last(stop: StopReason, error: Option<WireError>) -> ModelChunk {
    ModelChunk {
        stop: Some(stop),
        error,
        ..ModelChunk::default()
    }
}

/// Forward `stream`'s chunks until it ends, fails or is cancelled.
fn forward(mut stream: ChatStream) -> StreamStarter {
    Box::new(
        move |subscription: u64,
              outbox: mpsc::Sender<ServerMessage>,
              mut cancel: oneshot::Receiver<()>| {
            tokio::spawn(async move {
                let send = |chunk: ModelChunk| ServerMessage::Event {
                    subscription,
                    event: Event::ModelChunk(chunk),
                };
                let end = loop {
                    let next = tokio::select! {
                        // A dropped sender (the connection closing) is no cancel.
                        Ok(()) = &mut cancel => break last(StopReason::Cancelled, None),
                        next = stream.next() => next,
                    };
                    let chunk = match next {
                        None => break last(StopReason::End, None),
                        Some(Err(error)) => break last(StopReason::Error, Some(failure(error))),
                        Some(Ok(chunk)) => chunk,
                    };
                    let mut out = ModelChunk {
                        text: chunk.delta,
                        reasoning: chunk.reasoning.filter(|r| !r.is_empty()),
                        ..ModelChunk::default()
                    };
                    if let Some(reason) = chunk.finish_reason {
                        out.stop = Some(stop(&reason));
                        break out;
                    }
                    if out.text.is_empty() && out.reasoning.is_none() {
                        continue;
                    }
                    if outbox.send(send(out)).await.is_err() {
                        return;
                    }
                };
                // Dropping the stream here stops the server generating.
                drop(stream);
                let _ = outbox.send(send(end)).await;
            })
        },
    )
}

impl Gateway {
    async fn reply(
        &self,
        manifest: Option<&Manifest>,
        call: Call,
    ) -> Result<ModelsAnswer, WireError> {
        let who = caller(manifest);
        let reply =
            match call {
                Call::ModelsList => {
                    let models =
                        self.catalogue()
                            .installed_entries()
                            .filter(|e| match manifest {
                                None => true,
                                Some(m) => m.capabilities.models.as_ref().is_some_and(|access| {
                                    access.permits(e.meta.id.as_str(), false)
                                }),
                            })
                            .map(info)
                            .collect();
                    Reply::Models(models)
                }
                Call::ModelsInfo { model } => {
                    let entry = self.catalogue().get(&model).ok_or_else(|| {
                        WireError::new(ErrorCode::UnknownModel, format!("no model {model}"))
                    })?;
                    isonomic_proto::auth::authorize_model(who, &model, false)?;
                    Reply::Model(detail(entry, self.state(entry)))
                }
                Call::ModelsStream(request) => {
                    let (model, stream) = self.chat(who, request).await?;
                    return Ok(ModelsAnswer::Stream {
                        model,
                        start: forward(stream),
                    });
                }
                Call::ModelsDownload(d) => {
                    let entry = self.catalogue().get(&d.model).ok_or_else(|| {
                        WireError::new(ErrorCode::UnknownModel, format!("no model {}", d.model))
                    })?;
                    let (total_bytes, start) = if self.catalogue().installed(entry) {
                        crate::downloads::already_installed(entry)
                    } else {
                        let dir = self.catalogue().dir().to_path_buf();
                        self.downloads.start(entry, (self.source)(entry), dir)
                    };
                    return Ok(ModelsAnswer::Download {
                        model: d.model,
                        total_bytes,
                        start,
                    });
                }
                Call::ModelsDownloadCancel(d) => {
                    self.downloads.cancel(&d.model)?;
                    Reply::Cancelled {}
                }
                Call::ModelsRecommended(r) => Reply::Recommended(self.recommend(r.profile)?),
                Call::ModelsEmbed { model, input } => {
                    let (model, vectors) = self.embed(who, model.as_deref(), input).await?;
                    Reply::Embeddings { model, vectors }
                }
                other => {
                    return Err(WireError::new(
                        ErrorCode::UnknownMethod,
                        format!("{} is not a models method", other.method()),
                    ))
                }
            };
        Ok(ModelsAnswer::Reply(reply))
    }
}

impl Gateway {
    /// `models.recommended`: the first model for a profile (the engine's
    /// own when none is named), or `insufficient_memory` below the floor.
    fn recommend(&self, profile: Option<String>) -> Result<Recommendation, WireError> {
        let profile = profile.unwrap_or_else(|| self.machine.profile.clone());
        if !["low", "standard", "high"].contains(&profile.as_str()) {
            let message = format!("no profile {profile:?}; low, standard or high");
            return Err(WireError::new(ErrorCode::BadRequest, message));
        }
        let total = self.machine.total_memory_bytes;
        if total < crate::MEMORY_FLOOR {
            return Err(WireError::memory(MemoryRefusal {
                required_bytes: crate::MEMORY_FLOOR,
                // The machine's total memory: the floor is about the
                // machine, not what is free now.
                available_bytes: total,
                reason: "this machine has less memory than any local model needs".into(),
            }));
        }
        let entry = self.catalogue().first_model(profile == "low");
        Ok(Recommendation {
            model: entry.meta.id.as_str().to_string(),
            profile,
            size_bytes: Some(entry.meta.size_bytes).filter(|s| *s > 0),
        })
    }
}

impl broker::Models for Gateway {
    fn answer<'a>(
        &'a self,
        manifest: Option<&'a Manifest>,
        call: Call,
    ) -> BoxFuture<'a, Result<ModelsAnswer, WireError>> {
        Box::pin(self.reply(manifest, call))
    }
}
