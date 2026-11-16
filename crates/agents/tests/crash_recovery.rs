//! The engine dies mid-turn and is started again on the same Capsule: the
//! interrupted turn is sealed, the session takes the next message, and the
//! socket binds again. The first engine runs on its own runtime, dropped
//! mid-reply with nothing shut down, as a crash leaves it; the model server
//! is the inference testkit's fake, outliving both.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agents::{deps::EngineLocale, EngineScopes, GatewayModel, Parts, SqliteSessionStore};
use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, RamDenial, RamGate, SystemClock};
use gateway::Gateway;
use isonomic_client::{unix_connector, AppClient, Client, Options, SessionStream, SessionUpdate};
use isonomic_inference_testkit::{write_script, FakeModelServer};
use isonomic_model_library::Manifest;
use isonomic_proto::agents::SessionId;
use isonomic_proto::manifest::AgentsAccess;
use isonomic_proto::Capabilities;
use registry::GrantStore;
use tokio::runtime::Runtime;

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

/// An engine on the Capsule at `root`, as the engine binary assembles it;
/// Chat's client on it.
async fn engine(root: &Path, port: u16) -> AppClient {
    let paths = CapsulePaths::new(root);
    let models = root.join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("small.gguf"), [0u8; 16]).unwrap();
    let program = write_script(root, "stand-in", "#!/bin/sh\nexec /bin/sleep 3600\n");
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
        TokenKey::fixed([8; 32]),
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
    };
    let system = StackSystem::new(
        Arc::new(supervisor::Stack::new(None)),
        None,
        Arc::new(FixedHardware::with_total(16 << 30)),
        Profile::Standard,
    );
    // A crashed engine leaves its socket file behind; binding takes over.
    let bound = broker::bind(&paths.engine_socket()).expect("the socket binds again");
    tokio::spawn(broker::serve(bound, system, Arc::new(services)));
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
    shell.scoped(shell.grant(manifest).await.unwrap())
}

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
