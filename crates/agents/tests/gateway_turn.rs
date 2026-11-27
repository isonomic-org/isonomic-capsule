// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! One Chat turn served end to end by the engine's own parts: the broker,
//! the agents host on the SQLite log, and the model gateway, with a fake
//! model server in place of llama.cpp and a stand-in worker program.

use std::sync::Arc;
use std::time::Duration;

use agents::{deps::EngineLocale, EngineScopes, GatewayModel, Parts, SqliteSessionStore};
use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, RamDenial, RamGate, SystemClock};
use gateway::Gateway;
use isonomic_client::{unix_connector, Client, Options, SessionUpdate};
use isonomic_inference_testkit::{write_script, FakeModelServer};
use isonomic_model_library::Manifest;
use isonomic_proto::manifest::AgentsAccess;
use isonomic_proto::Capabilities;
use registry::GrantStore;

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

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_turn_streams_from_the_gateway_into_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("small.gguf"), [0u8; 16]).unwrap();
    let server = FakeModelServer::start().await;
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

    let apps = Arc::new(Apps::new(
        GrantStore::open(&paths.registry_db()).unwrap(),
        TokenKey::fixed([6; 32]),
    ));
    let host = agents::start(Parts {
        store: Arc::new(SqliteSessionStore::open(&paths.registry_db()).unwrap()),
        scopes: Arc::new(EngineScopes::new(Arc::clone(&apps), Arc::new(SystemClock))),
        model: Arc::new(GatewayModel::new(Arc::clone(&gateway))),
        locale: EngineLocale {
            zone: "UTC".into(),
            offset_minutes: 0,
            language: "en".into(),
        },
        profile: Profile::Standard,
        engine_tools: false,
        gateway: Some(Arc::clone(&gateway)),
        connections: None,
    })
    .await
    .unwrap();
    let services = Services {
        apps,
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(Arc::new(appdb::StandIn::default())),
        agents: Arc::new(host),
        models: gateway,
        identity: Arc::new(broker::identity::NoIdentity),
        connections: Arc::new(broker::NoConnections),
    };
    let stack = Arc::new(supervisor::Stack::new(None));
    let system = StackSystem::new(
        stack,
        None,
        Arc::new(FixedHardware::with_total(16 << 30)),
        Profile::Standard,
    );
    tokio::spawn(broker::serve(
        broker::bind(&paths.engine_socket()).unwrap(),
        system,
        Arc::new(services),
    ));

    let shell = Client::start(
        &tokio::runtime::Handle::current(),
        unix_connector(paths.engine_socket()),
        Options::new("test"),
    );
    let manifest = isonomic_proto::Manifest {
        id: "com.isonomic.chat".into(),
        name: "Chat".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            agents: Some(AgentsAccess {}),
            ..Default::default()
        },
        schemas: vec![],
    };
    let chat = shell.scoped(shell.grant(manifest).await.unwrap());
    let session = chat.agents().create("chat").await.unwrap();
    let mut stream = chat.agents().subscribe(&session, 0).await.unwrap();
    chat.agents()
        .send_text(&session, "Say hello")
        .await
        .unwrap();

    let mut reply = None;
    let mut ended = None;
    while ended.is_none() {
        let next = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
        let Some(SessionUpdate::Event(event)) = next.expect("no update within 10 s") else {
            continue;
        };
        let value = serde_json::to_value(&*event).unwrap();
        match value["type"].as_str() {
            Some("assistant") => reply = Some(value.to_string()),
            Some("turn.end") => ended = Some(value),
            _ => {}
        }
    }
    let ended = ended.unwrap();
    assert_eq!(ended["stop"], "done", "{ended}");
    let reply = reply.expect("an assistant event");
    assert!(reply.contains("Hello world"), "{reply}");

    // The gateway's request carried the user's words.
    let asked = server.requests_to("POST /v1/chat/completions");
    assert!(!asked.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_reply_continues_from_its_partial_text() {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("small.gguf"), [0u8; 16]).unwrap();
    let server = FakeModelServer::start().await;
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

    let apps = Arc::new(Apps::new(
        GrantStore::open(&paths.registry_db()).unwrap(),
        TokenKey::fixed([6; 32]),
    ));
    let host = agents::start(Parts {
        store: Arc::new(SqliteSessionStore::open(&paths.registry_db()).unwrap()),
        scopes: Arc::new(EngineScopes::new(Arc::clone(&apps), Arc::new(SystemClock))),
        model: Arc::new(GatewayModel::new(Arc::clone(&gateway))),
        locale: EngineLocale {
            zone: "UTC".into(),
            offset_minutes: 0,
            language: "en".into(),
        },
        profile: Profile::Standard,
        engine_tools: false,
        gateway: Some(Arc::clone(&gateway)),
        connections: None,
    })
    .await
    .unwrap();
    let services = Services {
        apps,
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(Arc::new(appdb::StandIn::default())),
        agents: Arc::new(host),
        models: gateway,
        identity: Arc::new(broker::identity::NoIdentity),
        connections: Arc::new(broker::NoConnections),
    };
    let stack = Arc::new(supervisor::Stack::new(None));
    let system = StackSystem::new(
        stack,
        None,
        Arc::new(FixedHardware::with_total(16 << 30)),
        Profile::Standard,
    );
    tokio::spawn(broker::serve(
        broker::bind(&paths.engine_socket()).unwrap(),
        system,
        Arc::new(services),
    ));

    let shell = Client::start(
        &tokio::runtime::Handle::current(),
        unix_connector(paths.engine_socket()),
        Options::new("test"),
    );
    let manifest = isonomic_proto::Manifest {
        id: "com.isonomic.chat".into(),
        name: "Chat".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            agents: Some(AgentsAccess {}),
            ..Default::default()
        },
        schemas: vec![],
    };
    let chat = shell.scoped(shell.grant(manifest).await.unwrap());
    let session = chat.agents().create("chat").await.unwrap();
    let mut stream = chat.agents().subscribe(&session, 0).await.unwrap();
    server.hold_stream_after_first_event();
    chat.agents()
        .send_text(&session, "Say hello")
        .await
        .unwrap();

    // The first text arrives, then the person stops the reply.
    let mut saw_text = false;
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
        match next.expect("no update within 10 s").expect("stream ended") {
            SessionUpdate::Delta(_) => {
                saw_text = true;
                chat.agents().cancel(&session).await.unwrap();
            }
            SessionUpdate::Event(e) => {
                let value = serde_json::to_value(&*e).unwrap();
                if value["type"] == "turn.end" {
                    assert!(saw_text, "the stop came before any text: {value}");
                    break;
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    server.release_stream();

    // Continue: the model is asked to go on from the sealed partial reply.
    chat.agents().continue_reply(&session).await.unwrap();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
        if let SessionUpdate::Event(e) = next.expect("no update within 10 s").expect("stream ended")
        {
            if serde_json::to_value(&*e).unwrap()["type"] == "turn.end" {
                break;
            }
        }
    }
    let asked = server.requests_to("POST /v1/chat/completions");
    let body: serde_json::Value = serde_json::from_str(&asked.last().unwrap().body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "assistant", "{body}");
    assert!(
        last["content"].as_str().unwrap().starts_with("Hello"),
        "{body}"
    );
}
