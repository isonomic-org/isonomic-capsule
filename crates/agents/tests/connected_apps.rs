// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Connected apps in a chat turn, end to end: the engine's connections
//! start a stand-in tool server over standard input and output, the chat
//! session is offered the connected-app tools, the model (scripted) calls
//! `connection_read`, and the server's answer reaches the model as the
//! tool's result. `connection_act` on a tool that changes something waits
//! for the person's approval instead of running. Hermetic: no network.

use std::sync::Arc;
use std::time::Duration;

use agents::{deps::EngineLocale, EngineScopes, Parts, SqliteSessionStore};
use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, SystemClock};
use connections::{Connections, MemoryVault, NewConnection, Server, State};
use harness_host::ScriptedModel;
use isonomic_client::{unix_connector, Client, Options, SessionUpdate};
use isonomic_proto::manifest::AgentsAccess;
use isonomic_proto::{Capabilities, Manifest};
use registry::GrantStore;

const SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"stand-in","version":"1"}}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"find_notes","description":"Find notes.","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}},{"name":"send_note","description":"Send a note.","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"name":"send_note"'*)
      echo sent >> "$0.sent"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"sent"}]}}\n' "$id" ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"Lunch with Ana on Friday"}]}}\n' "$id" ;;
  esac
done
"#;

fn kind(update: &SessionUpdate) -> String {
    match update {
        SessionUpdate::Event(e) => serde_json::to_value(&**e).unwrap()["type"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        SessionUpdate::Delta(_) => "delta".into(),
        SessionUpdate::Ended(_) => "ended".into(),
        SessionUpdate::Gap { .. } => "gap".into(),
    }
}

async fn until(stream: &mut isonomic_client::SessionStream, wanted: &str) -> Vec<String> {
    let mut seen = Vec::new();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
        let update = next
            .unwrap_or_else(|_| panic!("no {wanted} within 10 s: {seen:?}"))
            .expect("stream ended");
        let k = kind(&update);
        let done = k.starts_with(wanted);
        seen.push(k);
        if done {
            return seen;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_turn_reads_through_a_connection_and_asks_before_acting() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let script = dir.path().join("stand-in.sh");
    std::fs::write(&script, SERVER).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let options = connections::Options {
        child_env: vec![("PATH".into(), "/usr/bin:/bin".into())],
        ..connections::Options::default()
    };
    let list = Connections::open(dir.path(), Arc::new(MemoryVault::default()), options).unwrap();
    let notes = list
        .add(NewConnection {
            name: "Notes".into(),
            key: None,
            server: Server::Local {
                command: "/bin/sh".into(),
                args: vec![script.to_string_lossy().into_owned()],
            },
            token: None,
        })
        .unwrap();
    for _ in 0..500 {
        if list.get(&notes.id).map(|c| c.state) == Some(State::Ready) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(list.get(&notes.id).unwrap().state, State::Ready);

    let apps = Arc::new(Apps::new(
        GrantStore::open(&paths.registry_db()).unwrap(),
        TokenKey::fixed([6; 32]),
    ));
    let model = Arc::new(ScriptedModel::default());
    let parts = Parts {
        store: Arc::new(SqliteSessionStore::open(&paths.registry_db()).unwrap()),
        scopes: Arc::new(EngineScopes::new(Arc::clone(&apps), Arc::new(SystemClock))),
        model: model.clone(),
        locale: EngineLocale {
            zone: "UTC".into(),
            offset_minutes: 0,
            language: "en".into(),
        },
        profile: Profile::Standard,
        engine_tools: false,
        gateway: None,
        connections: Some(Arc::new(connections::service::Service(Arc::clone(&list)))),
    };
    let host = agents::start(parts).await.unwrap();
    let services = Services {
        apps,
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(Arc::new(appdb::StandIn::default())),
        agents: Arc::new(host),
        models: Arc::new(broker::NoModels),
        identity: Arc::new(broker::identity::NoIdentity),
        connections: Arc::new(connections::wire::Wire(Arc::clone(&list))),
    };
    let system = StackSystem::new(
        Arc::new(supervisor::Stack::new(None)),
        None,
        Arc::new(FixedHardware::with_total(16 << 30)),
        Profile::Standard,
    );
    let bound = broker::bind(&paths.engine_socket()).unwrap();
    tokio::spawn(broker::serve(bound, system, Arc::new(services)));
    let shell = Client::start(
        &tokio::runtime::Handle::current(),
        unix_connector(paths.engine_socket()),
        Options::new("test"),
    );
    let manifest = Manifest {
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

    let read = serde_json::json!({"connection": notes.id, "operation": "find_notes",
                                  "input": {"q": "Friday"}});
    model.calls(&[("connection_read", &read.to_string())]);
    model.text("You have lunch with Ana on Friday.");
    let session = chat.agents().create("chat").await.unwrap();
    let mut stream = chat.agents().subscribe(&session, 0).await.unwrap();
    chat.agents()
        .send_text(&session, "What is on Friday?")
        .await
        .unwrap();
    until(&mut stream, "turn.end").await;

    // The tools were offered, and the server's answer reached the model.
    let requests = model.requests.lock().unwrap().clone();
    let offered: Vec<_> = requests[0].tools.iter().map(|t| t.name.as_str()).collect();
    for tool in ["connections", "connection_read", "connection_act"] {
        assert!(offered.contains(&tool), "{offered:?}");
    }
    let answer = serde_json::to_string(&requests[1].messages).unwrap();
    assert!(answer.contains("Lunch with Ana on Friday"), "{answer}");

    // Acting asks first: nothing is sent while the approval waits.
    let act = serde_json::json!({"connection": notes.id, "operation": "send_note",
                                 "input": {"to": "Ana"}});
    model.calls(&[("connection_act", &act.to_string())]);
    chat.agents()
        .send_text(&session, "Tell Ana I'm in.")
        .await
        .unwrap();
    let seen = until(&mut stream, "approval").await;
    assert!(seen.iter().any(|k| k.starts_with("approval")), "{seen:?}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !dir.path().join("stand-in.sh.sent").exists(),
        "it ran without asking"
    );
}
