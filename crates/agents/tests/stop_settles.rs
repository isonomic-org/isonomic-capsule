//! A stopped reply lets go of its slot before the next reply is sent: the
//! stop drops its request at once (not at its next token), and the next
//! reply waits about one token gap for the model server to free the slot
//! that holds the conversation. A reply with no stop before it never waits.
//! Against the fake model server and a stand-in worker program.

use std::sync::Arc;
use std::time::{Duration, Instant};

use agents::settle::MOST;
use agents::GatewayModel;
use capsule_core::{RamDenial, RamGate};
use gateway::Gateway;
use harness_core::future::BoxStream;
use harness_core::ErrorInfo;
use harness_host::{CancelToken, ModelClient};
use harness_log::{ModelEvent, ModelRequest};
use isonomic_inference_testkit::{write_script, FakeModelServer};
use isonomic_model_library::Manifest;

struct Open;

impl RamGate for Open {
    fn admit(&self, _: u64) -> Result<(), RamDenial> {
        Ok(())
    }
}

type Events = BoxStream<'static, Result<ModelEvent, ErrorInfo>>;

async fn next(stream: &mut Events) -> Option<Result<ModelEvent, ErrorInfo>> {
    std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await
}

fn reply() -> ModelRequest {
    serde_json::from_value(serde_json::json!({
        "model": "local:default",
        "params": {"max_output_tokens": 256, "reasoning": "off"},
        "system": "You are a helpful assistant.",
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Plan a day in Lisbon"}]}]
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_drops_the_reply_at_once_and_the_next_reply_waits_for_its_slot() {
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("small.gguf"), [0u8; 16]).unwrap();
    let hash = "0".repeat(64);
    let catalogue = Manifest::from_json(&format!(
        r#"{{"default_chat":"chat-small","models":[{{"id":"chat-small","name":"Chat small","file_name":"small.gguf","source_url":"https://example.invalid/small.gguf","sha256":"{hash}","size_bytes":16,"context_length":4096,"capabilities":["chat"],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}]}}"#
    ))
    .unwrap();
    let server = FakeModelServer::start().await;
    let port = server.port();
    let program = write_script(dir.path(), "stand-in", "#!/bin/sh\nexec /bin/sleep 3600\n");
    let gateway = Arc::new(
        Gateway::new(catalogue, models, Some(program), Arc::new(Open), false).with_configure(
            Box::new(move |config| {
                config.port = Some(port);
                config.idle_unload = None;
                config.probe_interval = Duration::from_millis(20);
            }),
        ),
    );
    let model = GatewayModel::new(Arc::clone(&gateway));

    // A reply with no stop before it is sent at once (the worker starts).
    let mut warm = model.stream(reply(), CancelToken::default());
    while let Some(item) = tokio::time::timeout(Duration::from_secs(30), next(&mut warm))
        .await
        .expect("the first reply")
    {
        item.unwrap();
    }

    // The person stops a reply while it is being written.
    server.hold_stream_after_first_event();
    let cancel = CancelToken::default();
    let mut stopped = model.stream(reply(), cancel.clone());
    let first = tokio::time::timeout(Duration::from_secs(5), next(&mut stopped))
        .await
        .expect("its first words")
        .unwrap();
    assert!(matches!(first, Ok(ModelEvent::Text(_))), "{first:?}");
    cancel.cancel();
    gateway.reply_stopped();
    let ended = tokio::time::timeout(Duration::from_secs(1), next(&mut stopped))
        .await
        .expect("the stopped reply lets go at once, not at its next token");
    assert!(ended.is_none(), "{ended:?}");
    server.release_stream();

    // The next reply (a continue) waits for the stopped reply's slot.
    let started = Instant::now();
    let mut after = model.stream(reply(), CancelToken::default());
    tokio::time::timeout(Duration::from_secs(5), next(&mut after))
        .await
        .expect("the next reply")
        .unwrap()
        .unwrap();
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(100), "it waited {waited:?}");
    assert!(
        waited < MOST + Duration::from_millis(500),
        "it waited {waited:?}"
    );
    let log = capsule_core::log::tail();
    assert!(
        log.iter().any(|l| l.starts_with("reply: waited ")),
        "{log:?}"
    );

    // Long after a stop, a reply does not wait.
    tokio::time::sleep(MOST).await;
    let started = Instant::now();
    let mut later = model.stream(reply(), CancelToken::default());
    next(&mut later).await.unwrap().unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "{:?}",
        started.elapsed()
    );
}
