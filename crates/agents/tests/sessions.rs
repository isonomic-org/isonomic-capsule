//! The `agents` interface end to end: a real broker socket, the SDK client,
//! the harness host over the engine's SQLite log and token resolver, and a
//! scripted model in place of the gateway.

use std::sync::Arc;
use std::time::Duration;

use agents::{deps::EngineLocale, EngineScopes, Parts, SqliteSessionStore};
use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, SystemClock};
use harness_host::ScriptedModel;
use isonomic_client::{unix_connector, AppClient, Client, ClientError, Options, SessionUpdate};
use isonomic_proto::manifest::AgentsAccess;
use isonomic_proto::{Capabilities, ErrorCode, Manifest};
use registry::GrantStore;

struct Engine {
    _dir: tempfile::TempDir,
    shell: Client,
    model: Arc<ScriptedModel>,
}

fn manifest(name: &str) -> Manifest {
    Manifest {
        id: format!("com.isonomic.{name}"),
        name: name.into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            agents: Some(AgentsAccess {}),
            ..Default::default()
        },
        schemas: vec![],
    }
}

async fn engine() -> Engine {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let apps = Arc::new(Apps::new(
        GrantStore::open(&paths.registry_db()).unwrap(),
        TokenKey::fixed([5; 32]),
    ));
    let model = Arc::new(ScriptedModel::default());
    let parts = Parts {
        store: Arc::new(SqliteSessionStore::open(&paths.registry_db()).unwrap()),
        scopes: Arc::new(EngineScopes::new(Arc::clone(&apps), Arc::new(SystemClock))),
        model: model.clone(),
        locale: EngineLocale {
            zone: "Europe/Lisbon".into(),
            offset_minutes: 0,
            language: "en".into(),
        },
        profile: Profile::Standard,
        engine_tools: false,
    };
    let host = agents::start(parts).await.unwrap();
    let services = Services {
        apps,
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(Arc::new(appdb::StandIn::default())),
        agents: Arc::new(host),
        models: Arc::new(broker::NoModels),
    };
    let stack = Arc::new(supervisor::Stack::new(None));
    let hardware = Arc::new(FixedHardware::with_total(16 << 30));
    let system = StackSystem::new(stack, None, hardware, Profile::Standard);
    let bound = broker::bind(&paths.engine_socket()).unwrap();
    tokio::spawn(broker::serve(bound, system, Arc::new(services)));
    let shell = Client::start(
        &tokio::runtime::Handle::current(),
        unix_connector(paths.engine_socket()),
        Options::new("test"),
    );
    Engine {
        _dir: dir,
        shell,
        model,
    }
}

async fn app(engine: &Engine, name: &str) -> AppClient {
    let token = engine.shell.grant(manifest(name)).await.unwrap();
    engine.shell.scoped(token)
}

fn kind(update: &SessionUpdate) -> String {
    match update {
        SessionUpdate::Event(e) => serde_json::to_value(&**e).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_string(),
        SessionUpdate::Delta(_) => "delta".into(),
    }
}

/// Read updates until an event of `wanted` type arrives; return every
/// event type seen, in order, and the text of `assistant` events.
async fn until(stream: &mut isonomic_client::SessionStream, wanted: &str) -> (Vec<String>, String) {
    let mut seen = Vec::new();
    let mut text = String::new();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
        let update = next.expect("no update within 10 s").expect("stream ended");
        let k = kind(&update);
        if let SessionUpdate::Event(e) = &update {
            let value = serde_json::to_value(&**e).unwrap();
            if k == "assistant" {
                text.push_str(&value.to_string());
            }
        }
        let done = k == wanted;
        seen.push(k);
        if done {
            return (seen, text);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_turn_streams_a_reply_and_is_logged() {
    let engine = engine().await;
    let chat = app(&engine, "chat").await;
    engine.model.text("Sintra is lovely in spring.");

    let session = chat.agents().create("chat").await.unwrap();
    let mut stream = chat.agents().subscribe(&session, 0).await.unwrap();
    let (start, _) = until(&mut stream, "system").await;
    assert_eq!(start[0], "session.start");

    let queued = chat
        .agents()
        .send_text(&session, "Plan a day near Lisbon")
        .await;
    assert!(!queued.unwrap(), "nothing else was running");
    let (turn, text) = until(&mut stream, "turn.end").await;
    for wanted in ["turn.start", "user", "assistant"] {
        assert!(turn.iter().any(|k| k == wanted), "no {wanted} in {turn:?}");
    }
    assert!(text.contains("Sintra is lovely"), "{text}");

    // The log is durable: a new subscriber replays it from the store.
    let last = stream.last_seq();
    let mut replay = chat.agents().subscribe(&session, 0).await.unwrap();
    let (again, _) = until(&mut replay, "turn.end").await;
    assert_eq!(replay.last_seq(), last);
    let events = again.iter().filter(|k| k.as_str() != "delta").count();
    assert_eq!(events as u64, last);

    let snapshot = chat.agents().open(&session).await.unwrap();
    // The host may write more after the turn (a title, say).
    assert!(snapshot.summary.last_seq >= last);
    assert!(snapshot.turn_open.is_none());
    let listed = chat.agents().list(Default::default()).await.unwrap();
    assert_eq!(listed.0.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn another_apps_session_does_not_exist_for_it_and_purge_is_per_app() {
    let engine = engine().await;
    let chat = app(&engine, "chat").await;
    let notes = app(&engine, "notes").await;
    let session = chat.agents().create("chat").await.unwrap();

    for result in [
        notes.agents().open(&session).await.map(|_| ()),
        notes.agents().rename(&session, "mine now").await,
        notes.agents().cancel(&session).await,
    ] {
        match result {
            Err(ClientError::Engine(e)) => assert_eq!(e.code, ErrorCode::NotFound),
            other => panic!("expected not_found, got {other:?}"),
        }
    }
    assert!(notes
        .agents()
        .list(Default::default())
        .await
        .unwrap()
        .0
        .is_empty());

    let purged = notes.agents().purge(None).await.unwrap();
    assert_eq!(purged.sessions, 0);
    chat.agents().open(&session).await.unwrap();
    let purged = engine
        .shell
        .agents()
        .purge(Some("com.isonomic.chat"))
        .await
        .unwrap();
    assert_eq!(purged.sessions, 1);
    assert!(chat.agents().open(&session).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_and_continue_reach_the_host() {
    let engine = engine().await;
    let chat = app(&engine, "chat").await;
    let session = chat.agents().create("chat").await.unwrap();
    let mut stream = chat.agents().subscribe(&session, 0).await.unwrap();
    engine.model.text("One. Two. Three.");
    chat.agents().send_text(&session, "Count").await.unwrap();
    until(&mut stream, "turn.end").await;

    // Stopping an idle session is safe to repeat.
    chat.agents().cancel(&session).await.unwrap();
    chat.agents().cancel(&session).await.unwrap();
    // A finished reply has nothing to continue.
    match chat.agents().continue_reply(&session).await {
        Err(ClientError::Engine(e)) => assert_eq!(e.code, ErrorCode::ContinueRefused),
        Ok(()) => {
            let (seen, _) = until(&mut stream, "turn.end").await;
            assert!(seen.iter().any(|k| k == "turn.start"));
        }
        Err(other) => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn welcome_names_the_schema_the_host_writes() {
    let engine = engine().await;
    let _chat = app(&engine, "chat").await;
    let live = engine.shell.live();
    let schema = live.borrow().schema;
    assert_eq!(schema, Some(isonomic_proto::agents::SchemaVersion::CURRENT));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_result_for_a_call_never_issued_is_not_found() {
    let engine = engine().await;
    let chat = app(&engine, "chat").await;
    let session = chat.agents().create("chat").await.unwrap();
    let result = isonomic_proto::agents::ClientToolResult {
        session: session.clone(),
        call: serde_json::from_value(serde_json::json!("cal_9999")).unwrap(),
        content: vec![],
        error: None,
    };
    match chat.agents().tool_result(result).await {
        Err(ClientError::Engine(e)) => assert_eq!(e.code, ErrorCode::NotFound),
        other => panic!("a bogus call id was accepted: {other:?}"),
    }
}
