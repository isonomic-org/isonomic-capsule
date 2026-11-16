//! A person's prompt never waits behind a chat's title. A title request
//! waits for a quiet period after its turn, and a turn that begins
//! meanwhile, or while the title is being written, ends it at once with
//! [`agents::YIELDED`], its model request dropped. Replies never wait.
//!
//! The quiet period runs on tokio's paused clock; the streaming case uses
//! the fake model server and a stand-in worker program.

use std::sync::Arc;
use std::time::Duration;

use agents::{GatewayModel, TITLE_MODEL, YIELDED};
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

fn catalogue() -> Manifest {
    let hash = "0".repeat(64);
    Manifest::from_json(&format!(
        r#"{{"default_chat":"chat-small","models":[{{"id":"chat-small","name":"Chat small","file_name":"small.gguf","source_url":"https://example.invalid/small.gguf","sha256":"{hash}","size_bytes":16,"context_length":4096,"capabilities":["chat"],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}]}}"#
    ))
    .unwrap()
}

fn request(model: &str) -> ModelRequest {
    serde_json::from_value(serde_json::json!({
        "model": model,
        "params": {"max_output_tokens": 64, "reasoning": "off"},
        "system": "Name this conversation.",
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Plan a day in Lisbon"}]}]
    }))
    .unwrap()
}

/// A gateway with no model runtime: a request that reaches it fails at
/// once with "not installed", so a test sees whether it was sent.
fn bare() -> (tempfile::TempDir, Arc<Gateway>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("small.gguf"), [0u8; 16]).unwrap();
    let gateway = Gateway::new(
        catalogue(),
        dir.path().to_path_buf(),
        None,
        Arc::new(Open),
        false,
    );
    (dir, Arc::new(gateway))
}

type Events = BoxStream<'static, Result<ModelEvent, ErrorInfo>>;

/// The stream's next item (the harness's own stream trait).
async fn next(stream: &mut Events) -> Option<Result<ModelEvent, ErrorInfo>> {
    std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await
}

const QUIET: Duration = Duration::from_secs(2);

#[tokio::test(start_paused = true)]
async fn a_title_that_meets_a_turn_in_its_quiet_period_yields_unsent() {
    let (_dir, gateway) = bare();
    let model = GatewayModel::new(Arc::clone(&gateway)).with_title_quiet(QUIET);
    let mut title = model.stream(request(TITLE_MODEL), CancelToken::default());
    tokio::time::sleep(QUIET / 2).await;
    gateway.foreground_began();
    let first = next(&mut title).await.unwrap().unwrap_err();
    assert_eq!(first.code, YIELDED, "{first:?}");
}

#[tokio::test(start_paused = true)]
async fn a_title_is_sent_only_after_the_quiet_period_and_a_reply_at_once() {
    let (_dir, gateway) = bare();
    let model = GatewayModel::new(Arc::clone(&gateway)).with_title_quiet(QUIET);
    let started = tokio::time::Instant::now();
    let mut title = model.stream(request(TITLE_MODEL), CancelToken::default());
    let sent = next(&mut title).await.unwrap().unwrap_err();
    assert_ne!(sent.code, YIELDED, "{sent:?}");
    assert!(sent.message.contains("not installed"), "{sent:?}");
    assert!(
        started.elapsed() >= QUIET,
        "sent after {:?}",
        started.elapsed()
    );

    let started = tokio::time::Instant::now();
    let mut reply = model.stream(request("local:default"), CancelToken::default());
    let sent = next(&mut reply).await.unwrap().unwrap_err();
    assert!(sent.message.contains("not installed"), "{sent:?}");
    assert!(
        started.elapsed() < Duration::from_millis(1),
        "a reply never waits"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_title_being_written_yields_the_moment_a_turn_begins() {
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("small.gguf"), [0u8; 16]).unwrap();
    let server = FakeModelServer::start().await;
    server.hold_stream_after_first_event();
    let port = server.port();
    let program = write_script(dir.path(), "stand-in", "#!/bin/sh\nexec /bin/sleep 3600\n");
    let gateway = Arc::new(
        Gateway::new(catalogue(), models, Some(program), Arc::new(Open), false).with_configure(
            Box::new(move |config| {
                config.port = Some(port);
                config.idle_unload = None;
                config.probe_interval = Duration::from_millis(20);
            }),
        ),
    );
    let model = GatewayModel::new(Arc::clone(&gateway)).with_title_quiet(Duration::from_millis(10));
    let mut title = model.stream(request(TITLE_MODEL), CancelToken::default());
    // The title is being written: its first words are out, the rest held.
    let first = tokio::time::timeout(Duration::from_secs(30), next(&mut title))
        .await
        .expect("the title starts")
        .unwrap();
    assert!(matches!(first, Ok(ModelEvent::Text(_))), "{first:?}");
    gateway.foreground_began();
    let ended = tokio::time::timeout(Duration::from_secs(5), next(&mut title))
        .await
        .expect("the title gives way at once")
        .unwrap();
    assert_eq!(ended.unwrap_err().code, YIELDED);
    server.release_stream();
}
