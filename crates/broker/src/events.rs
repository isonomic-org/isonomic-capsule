// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `events` interface (SDK request R4): live messages between an app's
//! tabs and windows.
//!
//! One broadcast channel per scope (an app's id, or the shell's), shared by
//! every connection. Nothing is stored or replayed. A subscriber that falls
//! behind misses events rather than slowing the publisher. `data` is the
//! app's own JSON: it is passed through untouched and never logged.

use std::collections::HashMap;
use std::sync::Mutex;

use isonomic_proto::events::{matches, valid_topic};
use isonomic_proto::{AppEvent, ErrorCode, Event, Json, ServerMessage, WireError};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// Events a subscriber may fall behind by before it starts missing them.
const QUEUE: usize = 256;

/// The scope name of the shell's own events. App ids always have dots, so
/// it cannot collide with one.
const SHELL_SCOPE: &str = "shell";

#[derive(Default)]
pub struct Events {
    scopes: Mutex<HashMap<String, broadcast::Sender<AppEvent>>>,
}

fn bad_topic(topic: &str) -> WireError {
    WireError::new(
        ErrorCode::BadRequest,
        format!("topic {topic:?} must be lowercase letters, digits, dots, dashes and underscores"),
    )
}

/// The scope a call reaches: the app it names (only the shell may name
/// one; `proto::auth` has checked), else the caller's own.
fn scope(caller: Option<&str>, named: Option<String>) -> String {
    named.unwrap_or_else(|| caller.unwrap_or(SHELL_SCOPE).to_string())
}

impl Events {
    fn channel(&self, scope: &str) -> broadcast::Sender<AppEvent> {
        let mut scopes = self.scopes.lock().unwrap_or_else(|p| p.into_inner());
        // Drop channels nobody listens on, so scopes do not pile up.
        scopes.retain(|name, sender| name == scope || sender.receiver_count() > 0);
        scopes
            .entry(scope.to_string())
            .or_insert_with(|| broadcast::channel(QUEUE).0)
            .clone()
    }

    /// `events.publish`: hand the event to the scope's current subscribers.
    pub fn publish(
        &self,
        caller: Option<&str>,
        named: Option<String>,
        topic: String,
        data: Json,
        sender: Option<String>,
    ) -> Result<(), WireError> {
        if !valid_topic(&topic) {
            return Err(bad_topic(&topic));
        }
        let event = AppEvent {
            topic,
            data,
            sender,
        };
        // No receivers is not an error: events are live only.
        let _ = self.channel(&scope(caller, named)).send(event);
        Ok(())
    }

    /// `events.subscribe`: start listening now, so nothing published after
    /// the reply is missed. Start the returned listener once the reply is
    /// sent, so no event goes out before it.
    pub fn subscribe(
        &self,
        caller: Option<&str>,
        named: Option<String>,
        topics: Vec<String>,
    ) -> Result<Listener, WireError> {
        if let Some(bad) = topics.iter().find(|t| !valid_topic(t)) {
            return Err(bad_topic(bad));
        }
        let receiver = self.channel(&scope(caller, named)).subscribe();
        Ok(Listener { receiver, topics })
    }
}

/// A subscription that is listening but not yet forwarding.
pub struct Listener {
    receiver: broadcast::Receiver<AppEvent>,
    topics: Vec<String>,
}

impl Listener {
    /// Forward matching events to `outbox` as `app_event`s on
    /// `subscription`, until either side goes away.
    pub fn start(self, subscription: u64, outbox: mpsc::Sender<ServerMessage>) -> JoinHandle<()> {
        let Listener {
            mut receiver,
            topics,
        } = self;
        tokio::spawn(async move {
            loop {
                let event = match receiver.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if !matches(&topics, &event.topic) {
                    continue;
                }
                let message = ServerMessage::Event {
                    subscription,
                    event: Event::AppEvent(event),
                };
                if outbox.send(message).await.is_err() {
                    return;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(value: serde_json::Value) -> Json {
        Json::from(value)
    }

    #[tokio::test]
    async fn scopes_are_apart_and_the_shell_can_name_an_apps() {
        let events = Events::default();
        let (tx, mut rx) = mpsc::channel(8);
        let listener = events
            .subscribe(Some("com.isonomic.a"), None, vec![])
            .unwrap();
        let _task = listener.start(7, tx);

        let other = Some("com.isonomic.b");
        events
            .publish(other, None, "t".into(), json(serde_json::json!(1)), None)
            .unwrap();
        let named = Some("com.isonomic.a".to_string());
        events
            .publish(None, named, "t".into(), json(serde_json::json!(2)), None)
            .unwrap();

        let Some(ServerMessage::Event {
            subscription,
            event: Event::AppEvent(event),
        }) = rx.recv().await
        else {
            panic!()
        };
        assert_eq!((subscription, event.data), (7, json(serde_json::json!(2))));
    }

    #[test]
    fn a_bad_topic_is_refused() {
        let events = Events::default();
        let refused = events.publish(None, None, "Bad Topic".into(), Json::default(), None);
        assert_eq!(refused.unwrap_err().code, ErrorCode::BadRequest);
        let refused = events.subscribe(None, None, vec!["".into()]);
        assert_eq!(refused.err().unwrap().code, ErrorCode::BadRequest);
    }
}
