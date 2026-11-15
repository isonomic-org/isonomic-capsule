//! The `models` methods over the gateway, and a stream's chunks on the
//! wire: text and reasoning as they come, then exactly one chunk with
//! `stop`, whether the model ended, failed or was cancelled.

use broker::agents::BoxFuture;
use broker::models::{ModelsAnswer, StreamStarter};
use futures_util::StreamExt;
use isonomic_proto::auth::Caller;
use isonomic_proto::models::{ModelChunk, StopReason};
use isonomic_proto::{Call, ErrorCode, Event, Manifest, Reply, ServerMessage, WireError};
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

impl broker::Models for Gateway {
    fn answer<'a>(
        &'a self,
        manifest: Option<&'a Manifest>,
        call: Call,
    ) -> BoxFuture<'a, Result<ModelsAnswer, WireError>> {
        Box::pin(self.reply(manifest, call))
    }
}
