//! One client connection: version negotiation, then requests in, responses
//! and events out. A single writer task owns the socket's write half, so
//! frames never interleave.

use std::collections::HashMap;
use std::sync::Arc;

use isonomic_proto::auth::{authorize, Caller};
use isonomic_proto::io::{read_frame, write_message};
use isonomic_proto::lenient::{decode_client, FromClient};
use isonomic_proto::{
    negotiate, Call, ClientMessage, ErrorCode, Event, Outcome, Reply, ServerMessage, WireError,
};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::apps::Apps;
use crate::{System, ENGINE_VERSIONS};

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

/// Forward status changes to one subscriber until either side goes away.
fn forward(
    system: Arc<dyn System>,
    subscription: u64,
    outbox: mpsc::Sender<ServerMessage>,
) -> JoinHandle<()> {
    let mut changes = system.changes();
    tokio::spawn(async move {
        loop {
            let batch = match changes.recv().await {
                Ok(status) => vec![status],
                // The subscriber fell behind: resend everything so it converges.
                Err(RecvError::Lagged(_)) => system.status().services,
                Err(RecvError::Closed) => return,
            };
            for status in batch {
                let event = ServerMessage::Event {
                    subscription,
                    event: Event::ServiceStatus(status),
                };
                if outbox.send(event).await.is_err() {
                    return;
                }
            }
        }
    })
}

pub(crate) async fn handle(stream: UnixStream, system: Arc<dyn System>, apps: Arc<Apps>) {
    let (mut reader, writer) = stream.into_split();
    let (outbox, inbox) = mpsc::channel::<ServerMessage>(256);
    let writer_task = tokio::spawn(write_loop(writer, inbox));
    let mut subscriptions: HashMap<u64, JoinHandle<()>> = HashMap::new();
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
            Some(token) => match apps.manifest(token) {
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

        let mut then_shut_down = false;
        let mut then_forward = None;
        let outcome = match call {
            Call::SystemStatus => Outcome::Result(Reply::Status(system.status())),
            Call::SystemSubscribe => {
                // Listen first, snapshot second: no change can fall between.
                let subscription = next_subscription;
                next_subscription += 1;
                then_forward = Some((
                    subscription,
                    forward(Arc::clone(&system), subscription, outbox.clone()),
                ));
                Outcome::Result(Reply::Subscribed {
                    subscription,
                    snapshot: system.status(),
                })
            }
            Call::SystemUnsubscribe { subscription } => match subscriptions.remove(&subscription) {
                Some(task) => {
                    task.abort();
                    Outcome::Result(Reply::Unsubscribed {})
                }
                None => Outcome::Error(WireError::new(
                    ErrorCode::UnknownSubscription,
                    format!("no subscription {subscription}"),
                )),
            },
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
            Call::AppsGrant { manifest } => match apps.grant(&manifest) {
                Ok(token) => Outcome::Result(Reply::Granted {
                    app: manifest.id,
                    token,
                }),
                Err(error) => Outcome::Error(error),
            },
            other => Outcome::Error(WireError::new(
                ErrorCode::UnknownMethod,
                format!("this engine does not serve {} yet", other.method()),
            )),
        };
        let _ = outbox.send(ServerMessage::Response { id, outcome }).await;
        if let Some((subscription, task)) = then_forward {
            subscriptions.insert(subscription, task);
        }
        if then_shut_down {
            system.request_shutdown();
        }
    }

    for task in subscriptions.into_values() {
        task.abort();
    }
    drop(outbox);
    let _ = writer_task.await;
}
