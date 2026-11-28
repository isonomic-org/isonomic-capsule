// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tool calls on a local model: a request that offers tools reaches the
//! model server with them, and the server's two interleaved calls come
//! back as two whole harness tool calls and a stop for tool use. Against
//! the fake model server and a stand-in worker program.

use std::sync::Arc;
use std::time::Duration;

use agents::GatewayModel;
use capsule_core::{RamDenial, RamGate};
use gateway::Gateway;
use harness_core::future::BoxStream;
use harness_core::ErrorInfo;
use harness_host::{CancelToken, ModelClient};
use harness_log::{ModelEvent, ModelRequest, ModelStop};
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

#[tokio::test(flavor = "multi_thread")]
async fn interleaved_calls_come_back_whole_and_stop_for_tool_use() {
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("small.gguf"), [0u8; 16]).unwrap();
    let hash = "0".repeat(64);
    let catalogue = Manifest::from_json(&format!(
        r#"{{"default_chat":"chat-small","models":[{{"id":"chat-small","name":"Chat small","file_name":"small.gguf","source_url":"https://example.invalid/small.gguf","sha256":"{hash}","size_bytes":16,"context_length":4096,"capabilities":["chat"],"tags":["tools"],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}]}}"#
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
    let model = GatewayModel::new(gateway);
    assert!(model.caps("local:default").tools);
    let request: ModelRequest = serde_json::from_value(serde_json::json!({
        "model": "local:default",
        "params": {"max_output_tokens": 256, "reasoning": "off"},
        "system": "Use the tools.",
        "messages": [{"role": "user", "content": [{"type": "text", "text": "What is in a.txt?"}]}],
        "tools": [{"name": "read_file", "description": "Read a file.",
                   "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}},
                   "kind": "engine", "concurrency": "parallel"}]
    }))
    .unwrap();
    let mut events = model.stream(request, CancelToken::default());
    let mut seen = Vec::new();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(30), next(&mut events))
        .await
        .expect("the reply")
    {
        seen.push(item.unwrap());
    }
    let calls: Vec<_> = seen
        .iter()
        .filter_map(|e| match e {
            ModelEvent::ToolCall { tool, input } => Some((tool.as_str(), input.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        [
            ("read_file", r#"{"path":"a.txt"}"#),
            ("list_dir", r#"{"dir":"."}"#)
        ]
    );
    assert!(
        matches!(
            seen.last(),
            Some(ModelEvent::Done {
                stop: ModelStop::ToolUse,
                ..
            })
        ),
        "{seen:?}"
    );
    let sent = server.requests_to("POST /v1/chat/completions");
    let body: serde_json::Value = serde_json::from_str(&sent.last().unwrap().body).unwrap();
    assert_eq!(body["tools"][0]["function"]["name"], "read_file");
}
