// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! One client connection: version negotiation, then requests in, responses
//! and events out. A single writer task owns the socket's write half, so
//! frames never interleave.

use std::collections::HashMap;
use std::sync::Arc;

use isonomic_proto::auth::{authorize, Caller, Interface};
use isonomic_proto::connections::Connection;
use isonomic_proto::io::{read_frame, write_message};
use isonomic_proto::lenient::{decode_client, FromClient};
use isonomic_proto::{
    negotiate, Call, ClientMessage, ErrorCode, Event, Outcome, Reply, ServerMessage, Token,
    WireError,
};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::agents::{AgentsAnswer, Starter};
use crate::models::{ModelsAnswer, StreamStarter};
use crate::sql::Answer;
use crate::{Services, System, ENGINE_VERSIONS};

const DEFAULT_LOG_LINES: usize = 50;

fn error(id: u64, code: ErrorCode, message: impl Into<String>) -> ServerMessage {
    ServerMessage::Response {
        id,
        outcome: Outcome::Error(WireError::new(code, message)),
    }
}

async fn write_loop(mut writer: OwnedWriteHalf, mut outbox: mpsc::Receiver<ServerMessage>) {
    while let Some(message) = outbox.recv().await {
        if write_message(&mut writer, &message).await.is_err() {
            return;
        }
    }
}

/// Forward status changes, and each connection's changes, to one
/// subscriber until either side goes away.
fn forward(
    system: Arc<dyn System>,
    connections: Option<broadcast::Receiver<Connection>>,
    subscription: u64,
    outbox: mpsc::Sender<ServerMessage>,
) -> JoinHandle<()> {
    let mut changes = system.changes();
    let mut connections = connections;
    tokio::spawn(async move {
        loop {
            let next = async {
                match connections.as_mut() {
                    Some(rx) => Some(rx.recv().await),
                    None => std::future::pending().await,
                }
            };
            let events = tokio::select! {
                status = changes.recv() => match status {
                    Ok(status) => vec![Event::ServiceStatus(status)],
                    // The subscriber fell behind: resend everything so it converges.
                    Err(RecvError::Lagged(_)) => system
                        .status()
                        .services
                        .into_iter()
                        .map(Event::ServiceStatus)
                        .collect(),
                    Err(RecvError::Closed) => return,
                },
                Some(change) = next => match change {
                    Ok(connection) => vec![Event::Connection(connection)],
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => {
                        connections = None;
                        continue;
                    }
                },
            };
            for event in events {
                let event = ServerMessage::Event {
                    subscription,
                    event,
                };
                if outbox.send(event).await.is_err() {
                    return;
                }
            }
        }
    })
}

pub(crate) async fn handle(stream: UnixStream, system: Arc<dyn System>, services: Arc<Services>) {
    let (mut reader, writer) = stream.into_split();
    let (outbox, inbox) = mpsc::channel::<ServerMessage>(256);
    let writer_task = tokio::spawn(write_loop(writer, inbox));
    // Each subscription remembers the token that opened it: only that
    // caller may end it, even on the same connection.
    let mut subscriptions: HashMap<u64, (JoinHandle<()>, Option<Token>)> = HashMap::new();
    // Model streams end themselves on cancel, with a `stop: cancelled` chunk.
    let mut cancels: HashMap<u64, oneshot::Sender<()>> = HashMap::new();
    let mut next_subscription: u64 = 1;
    let mut greeted = false;

    loop {
        let body = match read_frame(&mut reader).await {
            Ok(Some(body)) => body,
            Ok(None) | Err(_) => break,
        };
        // The frame boundary is intact, so an unreadable message is answered
        // and the connection goes on.
        let message = match decode_client(&body) {
            FromClient::Message(message) => message,
            FromClient::Rejected { id, error } => {
                let outcome = Outcome::Error(error);
                let _ = outbox.send(ServerMessage::Response { id, outcome }).await;
                continue;
            }
            FromClient::Garbage(error) => {
                let outcome = Outcome::Error(error);
                let _ = outbox
                    .send(ServerMessage::Response { id: 0, outcome })
                    .await;
                continue;
            }
        };
        let (id, token, call) = match message {
            ClientMessage::Hello { versions, .. } => {
                let Some(version) = negotiate(&versions, ENGINE_VERSIONS) else {
                    let text = format!("this engine speaks versions {ENGINE_VERSIONS:?}");
                    let _ = outbox
                        .send(error(0, ErrorCode::UnsupportedVersion, text))
                        .await;
                    break;
                };
                greeted = true;
                let welcome = ServerMessage::Welcome {
                    version,
                    engine: env!("CARGO_PKG_VERSION").to_string(),
                    // The session event schema the agents host writes.
                    schema: Some(isonomic_proto::agents::SchemaVersion::CURRENT),
                };
                let _ = outbox.send(welcome).await;
                continue;
            }
            ClientMessage::Request { id, token, call } => (id, token, call),
        };
        if !greeted {
            let _ = outbox
                .send(error(id, ErrorCode::HelloRequired, "send hello first"))
                .await;
            continue;
        }

        // Who is calling: no token is the shell; a token is the app it was
        // issued to, with its approved manifest. Then the capability check,
        // before any work. Neither answer ever repeats the token.
        let manifest = match &token {
            None => None,
            Some(token) => match services.apps.manifest(token) {
                Ok(manifest) => Some(manifest),
                Err(error) => {
                    let outcome = Outcome::Error(error);
                    let _ = outbox.send(ServerMessage::Response { id, outcome }).await;
                    continue;
                }
            },
        };
        let caller = match &manifest {
            None => Caller::Shell,
            Some(manifest) => Caller::App {
                id: &manifest.id,
                capabilities: &manifest.capabilities,
            },
        };
        if let Err(error) = authorize(caller, &call) {
            let outcome = Outcome::Error(error);
            let _ = outbox.send(ServerMessage::Response { id, outcome }).await;
            continue;
        }
        // Then the person's grants, for an app's data calls (R7 step 4).
        if let Some(manifest) = &manifest {
            if let Err(error) = services.identity.check(manifest, &call).await {
                let outcome = Outcome::Error(error);
                let _ = outbox.send(ServerMessage::Response { id, outcome }).await;
                continue;
            }
        }

        let mut then_shut_down = false;
        let mut then_forward = None;
        // A subscription that starts forwarding once its reply is out.
        let mut then_listen: Option<(u64, Starter)> = None;
        let mut then_stream: Option<(u64, StreamStarter)> = None;
        let outcome = match call {
            Call::SystemStatus => Outcome::Result(Reply::Status(system.status())),
            Call::SystemSubscribe => {
                // Listen first, snapshot second: no change can fall between.
                let subscription = next_subscription;
                next_subscription += 1;
                then_forward = Some((
                    subscription,
                    forward(
                        Arc::clone(&system),
                        services.connections.changes(),
                        subscription,
                        outbox.clone(),
                    ),
                ));
                Outcome::Result(Reply::Subscribed {
                    subscription,
                    snapshot: system.status(),
                })
            }
            Call::SystemUnsubscribe { subscription }
            | Call::EventsUnsubscribe { subscription }
            | Call::SqlUnsubscribe { subscription }
            | Call::SessionsUnsubscribe { subscription } => {
                match subscriptions.get(&subscription) {
                    Some((_, owner)) if *owner == token => {
                        if let Some((task, _)) = subscriptions.remove(&subscription) {
                            task.abort();
                        }
                        Outcome::Result(Reply::Unsubscribed {})
                    }
                    _ => Outcome::Error(WireError::new(
                        ErrorCode::UnknownSubscription,
                        format!("no subscription {subscription}"),
                    )),
                }
            }
            Call::SystemLogs { service, limit } => {
                let limit = limit.map(|n| n as usize).unwrap_or(DEFAULT_LOG_LINES);
                match system.logs(&service, limit) {
                    Some(lines) => Outcome::Result(Reply::Logs { service, lines }),
                    None => Outcome::Error(WireError::new(
                        ErrorCode::UnknownService,
                        format!("no service named {service}"),
                    )),
                }
            }
            Call::SystemProfile => Outcome::Result(Reply::Profile(system.profile())),
            Call::SystemShutdown => {
                then_shut_down = true;
                Outcome::Result(Reply::ShuttingDown {})
            }
            Call::AppsGrant { manifest } => match services.apps.grant(&manifest) {
                Ok(token) => {
                    services.sql.granted(&manifest);
                    // The approval is recorded; the app's ID and grants
                    // follow it before the window hears it was granted.
                    match services.identity.granted(&manifest).await {
                        Ok(()) => Outcome::Result(Reply::Granted {
                            app: manifest.id,
                            token,
                        }),
                        Err(error) => Outcome::Error(error),
                    }
                }
                Err(error) => Outcome::Error(error),
            },
            Call::ModelsCancel { subscription } => match subscriptions.get(&subscription) {
                Some((task, owner))
                    if *owner == token
                        && !task.is_finished()
                        && cancels.contains_key(&subscription) =>
                {
                    if let Some(cancel) = cancels.remove(&subscription) {
                        let _ = cancel.send(());
                    }
                    // The task sends the last chunk and ends on its own.
                    subscriptions.remove(&subscription);
                    Outcome::Result(Reply::Cancelled {})
                }
                _ => Outcome::Error(WireError::new(
                    ErrorCode::UnknownSubscription,
                    format!("no stream {subscription}"),
                )),
            },
            call if call.interface() == Interface::Models => {
                match services.models.answer(manifest.as_ref(), call).await {
                    Ok(ModelsAnswer::Reply(reply)) => Outcome::Result(reply),
                    Ok(ModelsAnswer::Download {
                        model,
                        total_bytes,
                        start,
                    }) => {
                        let subscription = next_subscription;
                        next_subscription += 1;
                        then_listen = Some((subscription, start));
                        Outcome::Result(Reply::Downloading {
                            subscription,
                            model,
                            total_bytes,
                        })
                    }
                    Ok(ModelsAnswer::Stream { model, start }) => {
                        let subscription = next_subscription;
                        next_subscription += 1;
                        then_stream = Some((subscription, start));
                        Outcome::Result(Reply::Stream {
                            subscription,
                            model,
                        })
                    }
                    Err(error) => Outcome::Error(error),
                }
            }
            Call::IdentityGet(_) => match services.identity.get().await {
                Ok(identity) => Outcome::Result(Reply::Identity(identity)),
                Err(error) => Outcome::Error(error),
            },
            // The recovery kit: shell only (`authorize` above). The words
            // travel in these replies and are never logged here.
            Call::IdentityKitExport(params) => {
                match services.identity.kit_export(params.person_asked).await {
                    Ok(kit) => Outcome::Result(Reply::RecoveryKit(kit)),
                    Err(error) => Outcome::Error(error),
                }
            }
            Call::IdentityKitRestore(restore) => {
                match services.identity.kit_restore(restore).await {
                    Ok(restored) => Outcome::Result(Reply::KitRestored(restored)),
                    Err(error) => Outcome::Error(error),
                }
            }
            Call::IdentityKitSaved(_) => match services.identity.kit_saved().await {
                Ok(()) => Outcome::Result(Reply::Done {}),
                Err(error) => Outcome::Error(error),
            },
            Call::IdentityKitStatus(_) => match services.identity.kit_status().await {
                Ok(status) => Outcome::Result(Reply::KitStatus(status)),
                Err(error) => Outcome::Error(error),
            },
            call if call.interface() == Interface::Connections => {
                match services.connections.call(call).await {
                    Ok(reply) => Outcome::Result(reply),
                    Err(error) => Outcome::Error(error),
                }
            }
            call if call.interface() == Interface::Agents => {
                match services.agents.answer(manifest.as_ref(), call).await {
                    Ok(AgentsAnswer::Reply(reply)) => Outcome::Result(reply),
                    Ok(AgentsAnswer::Stream(start)) => {
                        let subscription = next_subscription;
                        next_subscription += 1;
                        then_listen = Some((subscription, start));
                        Outcome::Result(Reply::SessionStream { subscription })
                    }
                    Err(error) => Outcome::Error(error),
                }
            }
            call if call.interface() == Interface::Sql => {
                match services.sql.answer(manifest.as_ref(), call).await {
                    Ok(Answer::Reply(reply)) => Outcome::Result(reply),
                    Ok(Answer::Live(rows, live)) => {
                        let subscription = next_subscription;
                        next_subscription += 1;
                        let start: Starter = Box::new(move |s, out| live.start(s, out));
                        then_listen = Some((subscription, start));
                        Outcome::Result(Reply::LiveQuery { subscription, rows })
                    }
                    Err(error) => Outcome::Error(error),
                }
            }
            Call::EventsPublish {
                topic,
                data,
                sender,
                app,
            } => {
                let caller = manifest.as_ref().map(|m| m.id.as_str());
                match services.events.publish(caller, app, topic, data, sender) {
                    Ok(()) => Outcome::Result(Reply::Published {}),
                    Err(error) => Outcome::Error(error),
                }
            }
            Call::EventsSubscribe { topics, app } => {
                let caller = manifest.as_ref().map(|m| m.id.as_str());
                match services.events.subscribe(caller, app, topics) {
                    Ok(listener) => {
                        let subscription = next_subscription;
                        next_subscription += 1;
                        let start: Starter = Box::new(move |s, out| listener.start(s, out));
                        then_listen = Some((subscription, start));
                        Outcome::Result(Reply::Listening { subscription })
                    }
                    Err(error) => Outcome::Error(error),
                }
            }
            call if call.interface() == Interface::Blobs => {
                let app = manifest.as_ref().map(|m| m.id.clone());
                let services = Arc::clone(&services);
                let work = move || services.blobs.answer(app.as_deref(), call);
                match tokio::task::spawn_blocking(work).await {
                    Ok(Ok(reply)) => Outcome::Result(reply),
                    Ok(Err(error)) => Outcome::Error(error),
                    Err(_) => Outcome::Error(WireError::new(
                        ErrorCode::Internal,
                        "the blob store stopped answering",
                    )),
                }
            }
            other => Outcome::Error(WireError::new(
                ErrorCode::UnknownMethod,
                format!("this engine does not serve {} yet", other.method()),
            )),
        };
        let _ = outbox.send(ServerMessage::Response { id, outcome }).await;
        if let Some((subscription, task)) = then_forward {
            subscriptions.insert(subscription, (task, token.clone()));
        }
        // The reply is out, so no event can arrive before it.
        if let Some((subscription, start)) = then_listen {
            let task = start(subscription, outbox.clone());
            subscriptions.insert(subscription, (task, token.clone()));
        }
        if let Some((subscription, start)) = then_stream {
            let (cancel, cancelled) = oneshot::channel();
            let task = start(subscription, outbox.clone(), cancelled);
            cancels.insert(subscription, cancel);
            subscriptions.insert(subscription, (task, token.clone()));
        }
        if then_shut_down {
            system.request_shutdown();
        }
    }

    for (task, _) in subscriptions.into_values() {
        task.abort();
    }
    drop(outbox);
    let _ = writer_task.await;
}
