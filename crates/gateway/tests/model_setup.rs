// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The model setup and tools on local models, over a real socket:
//! `models.catalogue` with its labels for an injected machine,
//! `models.remove` of a model whose worker runs, and a stream that offers
//! tools. The fake model server stands in for the model server; nothing leaves
//! the machine.

use std::sync::Arc;
use std::time::Duration;

use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, RamDenial, RamGate};
use gateway::{Gateway, Machine};
use isonomic_client::{unix_connector, Client, ClientError, Options};
use isonomic_inference_testkit::{write_script, FakeModelServer};
use isonomic_model_library::Manifest;
use isonomic_proto::manifest::ModelAccess;
use isonomic_proto::models::{
    Message, ModelState, Role, StopReason, StreamRequest, ToolCall, ToolSpec,
};
use isonomic_proto::{Capabilities, ErrorCode};
use registry::GrantStore;

const SLEEPER: &str = "#!/bin/sh\nexec /bin/sleep 3600\n";
const GIB: u64 = 1 << 30;

struct Open;
impl RamGate for Open {
    fn admit(&self, _: u64) -> Result<(), RamDenial> {
        Ok(())
    }
}

fn manifest() -> Manifest {
    let hash = "0".repeat(64);
    let model = |id: &str, size: u64, caps: &str, extra: &str| {
        format!(
            r#"{{"id":"{id}","name":"{id}","file_name":"{id}.gguf","source_url":"https://example.invalid/{id}","sha256":"{hash}","size_bytes":{size},"context_length":4096,"capabilities":[{caps}],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}{extra}}}"#
        )
    };
    let row = r#","publisher":"Example Lab","about":"A small chat model.","min_ram_gb":8"#;
    Manifest::from_json(&format!(
        r#"{{"default_chat":"qwen-small","default_embeddings":"nomic-embed","models":[{},{},{}]}}"#,
        model("qwen-small", 16, r#""chat""#, row),
        model("nomic-embed", 8, r#""embeddings""#, ""),
        // Never installed: only its size and least memory matter, for the
        // label.
        model("huge", 20 * GIB, r#""chat""#, r#","min_ram_gb":32"#),
    ))
    .unwrap()
}

struct Rig {
    _dir: tempfile::TempDir,
    server: FakeModelServer,
    shell: Client,
}

async fn rig(total_memory_bytes: u64) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("qwen-small.gguf"), [0u8; 16]).unwrap();
    std::fs::write(models.join("nomic-embed.gguf"), [0u8; 8]).unwrap();
    let server = FakeModelServer::start().await;
    let port = server.port();
    let program = write_script(dir.path(), "stand-in", SLEEPER);
    let gateway = Gateway::new(manifest(), models, Some(program), Arc::new(Open), false)
        .with_machine(Machine {
            profile: Profile::from_total_memory(total_memory_bytes).to_string(),
            total_memory_bytes,
        })
        .with_configure(Box::new(move |config| {
            config.port = Some(port);
            config.idle_unload = None;
            config.probe_interval = Duration::from_millis(20);
        }));
    let services = Services {
        apps: Arc::new(Apps::new(
            GrantStore::in_memory().unwrap(),
            TokenKey::fixed([5; 32]),
        )),
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(Arc::new(appdb::StandIn::default())),
        agents: Arc::new(broker::NoAgents),
        models: Arc::new(gateway),
        identity: Arc::new(broker::identity::NoIdentity),
        connections: Arc::new(broker::NoConnections),
    };
    let stack = Arc::new(supervisor::Stack::new(None));
    let hardware = Arc::new(FixedHardware::with_total(total_memory_bytes));
    let system = StackSystem::new(stack, None, hardware, Profile::Standard);
    let bound = broker::bind(&paths.engine_socket()).unwrap();
    tokio::spawn(broker::serve(bound, system, Arc::new(services)));
    let shell = Client::start(
        &tokio::runtime::Handle::current(),
        unix_connector(paths.engine_socket()),
        Options::new("test"),
    );
    Rig {
        _dir: dir,
        server,
        shell,
    }
}

fn code<T>(result: Result<T, ClientError>) -> ErrorCode {
    match result {
        Err(ClientError::Engine(e)) => e.code,
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("expected a refusal"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_catalogue_labels_every_model_for_this_machine() {
    let rig = rig(16 * GIB).await;
    let entries = rig.shell.models().catalogue().await.unwrap();
    let ids: Vec<_> = entries.iter().map(|e| e.detail.info.id.as_str()).collect();
    assert_eq!(ids, ["qwen-small", "nomic-embed", "huge"]);
    let by = |id: &str| entries.iter().find(|e| e.detail.info.id == id).unwrap();
    assert_eq!(by("qwen-small").detail.state, ModelState::Stopped);
    assert_eq!(by("huge").detail.state, ModelState::NotInstalled);
    assert!(by("qwen-small").fits);
    assert!(by("nomic-embed").fits);
    assert!(!by("huge").fits);
    let reason = by("huge").fit_reason.clone().unwrap();
    assert!(reason.contains("This Mac has 16 GB"), "{reason}");
    assert!(by("qwen-small")
        .fit_reason
        .as_ref()
        .unwrap()
        .contains("16 GB"));
    assert!(reason.contains("at least 32 GB"), "{reason}");
    assert_eq!(by("qwen-small").publisher.as_deref(), Some("Example Lab"));
    assert_eq!(
        by("qwen-small").about.as_deref(),
        Some("A small chat model.")
    );
    assert_eq!(by("huge").publisher, None);
    assert_eq!(by("huge").about, None);

    // The model setup is the shell's.
    let manifest = isonomic_proto::Manifest {
        id: "com.isonomic.chat".into(),
        name: "Chat".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            models: Some(ModelAccess {
                any: true,
                ..Default::default()
            }),
            ..Default::default()
        },
        schemas: vec![],
    };
    let app = rig.shell.scoped(rig.shell.grant(manifest).await.unwrap());
    assert_eq!(
        code(app.models().catalogue().await),
        ErrorCode::CapabilityDenied
    );
    assert_eq!(
        code(app.models().remove("qwen-small").await),
        ErrorCode::CapabilityDenied
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn below_the_memory_floor_nothing_is_recommended() {
    let rig = rig(8 * GIB).await;
    let entries = rig.shell.models().catalogue().await.unwrap();
    assert_eq!(entries.len(), 3);
    for e in entries {
        assert!(!e.fits, "{}", e.detail.info.id);
        let reason = e.fit_reason.unwrap();
        assert!(reason.contains("This Mac has 8 GB"), "{reason}");
    }
}

fn read_file() -> ToolSpec {
    ToolSpec {
        name: "read_file".into(),
        description: "Read a file".into(),
        schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_that_offers_tools_carries_the_calls_and_stops_for_them() {
    let rig = rig(16 * GIB).await;
    let request = StreamRequest {
        messages: vec![
            Message::user("Read a.txt"),
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_0".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"b.txt"}"#.into(),
                }],
                tool_call_id: None,
            },
            Message {
                role: Role::Tool,
                content: "the text of b".into(),
                tool_calls: vec![],
                tool_call_id: Some("call_0".into()),
            },
        ],
        tools: vec![read_file()],
        ..Default::default()
    };
    let mut stream = rig.shell.models().stream(request).await.unwrap();
    let (mut deltas, mut stops) = (Vec::new(), Vec::new());
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("no chunk within 10 s")
    {
        let chunk = chunk.unwrap();
        deltas.extend(chunk.tool_calls);
        stops.extend(chunk.stop);
    }
    assert_eq!(stops, [StopReason::ToolCalls]);
    let args = |index: u32| -> String {
        deltas
            .iter()
            .filter(|d| d.index == index)
            .map(|d| d.arguments.as_str())
            .collect()
    };
    assert_eq!(args(0), r#"{"path":"a.txt"}"#);
    assert_eq!(args(1), r#"{"dir":"."}"#);
    assert_eq!(deltas[0].id.as_deref(), Some("call_a"));

    // The server got the tool, the earlier call and its result.
    let sent = rig.server.requests_to("POST /v1/chat/completions");
    let body: serde_json::Value = serde_json::from_str(&sent.last().unwrap().body).unwrap();
    assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_0");
    assert_eq!(body["messages"][2]["role"], "tool");
    assert_eq!(body["messages"][2]["tool_call_id"], "call_0");
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_loaded_model_unloads_it_first() {
    let rig = rig(16 * GIB).await;
    let models = rig.shell.models();
    let hello = StreamRequest {
        model: Some("qwen-small".into()),
        messages: vec![Message::user("hi")],
        ..Default::default()
    };
    let text = models.stream(hello.clone()).await.unwrap().text().await;
    assert_eq!(text.unwrap(), "Hello world");
    assert_eq!(
        models.info("qwen-small").await.unwrap().state,
        ModelState::Ready
    );
    models.remove("qwen-small").await.unwrap();
    assert_eq!(
        models.info("qwen-small").await.unwrap().state,
        ModelState::NotInstalled
    );
    assert_eq!(
        code(models.stream(hello).await),
        ErrorCode::ModelUnavailable
    );
}
