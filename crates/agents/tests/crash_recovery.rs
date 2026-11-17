// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The engine dies mid-turn and is started again on the same Capsule: the
//! interrupted turn is sealed, the session takes the next message, and the
//! socket binds again. The first engine runs on its own runtime, dropped
//! mid-reply with nothing shut down, as a crash leaves it; the model server
//! is the inference testkit's fake, outliving both.

mod support;

use std::time::Duration;

use isonomic_client::{SessionStream, SessionUpdate};
use isonomic_inference_testkit::FakeModelServer;
use isonomic_proto::agents::SessionId;
use support::engine;
use tokio::runtime::Runtime;

/// Event types until one of `wanted` arrives, and that event.
async fn until(stream: &mut SessionStream, wanted: &str) -> (Vec<String>, serde_json::Value) {
    let mut seen = Vec::new();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
        match next.expect("no update within 10 s").expect("stream ended") {
            SessionUpdate::Event(e) => {
                let value = serde_json::to_value(&*e).unwrap();
                let kind = value["type"].as_str().unwrap_or("").to_string();
                seen.push(kind.clone());
                if kind == wanted {
                    return (seen, value);
                }
            }
            SessionUpdate::Delta(_) => seen.push("delta".into()),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn an_engine_that_dies_mid_turn_leaves_the_session_usable() {
    let dir = tempfile::tempdir().unwrap();
    let models = Runtime::new().unwrap();
    let server = models.block_on(FakeModelServer::start());
    let port = server.port();
    server.hold_stream_after_first_event();

    // The first engine streams the start of a reply, then dies.
    let first = Runtime::new().unwrap();
    let session: SessionId = first.block_on(async {
        let chat = engine(dir.path(), port).await;
        let session = chat.agents().create("chat").await.unwrap();
        let mut stream = chat.agents().subscribe(&session, 0).await.unwrap();
        chat.agents()
            .send_text(&session, "Say hello")
            .await
            .unwrap();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
            if let SessionUpdate::Delta(_) = next.expect("no reply began").expect("stream ended") {
                break;
            }
        }
        session
    });
    drop(first);
    server.release_stream();

    // The next engine opens the same Capsule.
    let second = Runtime::new().unwrap();
    second.block_on(async {
        let chat = engine(dir.path(), port).await;
        let mut stream = chat.agents().subscribe(&session, 0).await.unwrap();
        let (_, sealed) = until(&mut stream, "turn.end").await;
        println!("sealed: {}", sealed["stop"]);
        assert_ne!(sealed["stop"], "done", "the interrupted turn: {sealed}");

        chat.agents()
            .send_text(&session, "Say hello again")
            .await
            .unwrap();
        let (seen, end) = until(&mut stream, "turn.end").await;
        assert_eq!(end["stop"], "done", "{end} after {seen:?}");
        assert!(seen.iter().any(|k| k == "assistant"), "{seen:?}");
        let listed = chat.agents().list(Default::default()).await.unwrap().0;
        assert!(
            listed.iter().any(|s| s.id == session),
            "the session is listed"
        );
    });
}
