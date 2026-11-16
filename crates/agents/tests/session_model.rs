//! A session created with a model a person picked: checked with the
//! stream rule, then run on that model; a name that slips through fails
//! its turn with a reason. The gateway runs against a fake model server.

use std::sync::Arc;
use std::time::Duration;

use agents::{deps::EngineLocale, EngineScopes, GatewayModel, Parts, SqliteSessionStore};
use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, RamDenial, RamGate, SystemClock};
use gateway::Gateway;
use isonomic_client::{unix_connector, AppClient, Client, ClientError, Options, SessionUpdate};
use isonomic_inference_testkit::{write_script, FakeModelServer};
use isonomic_model_library::Manifest;
use isonomic_proto::agents::CreateSession;
use isonomic_proto::manifest::{AgentsAccess, ModelAccess};
use isonomic_proto::{Capabilities, ErrorCode};
use registry::GrantStore;

struct Open;
impl RamGate for Open {
    fn admit(&self, _: u64) -> Result<(), RamDenial> {
        Ok(())
    }
}

fn catalogue() -> Manifest {
    let hash = "0".repeat(64);
    let model = |id: &str, file: &str| {
        format!(
            r#"{{"id":"{id}","name":"{id}","file_name":"{file}","source_url":"https://example.invalid/{file}","sha256":"{hash}","size_bytes":16,"context_length":4096,"capabilities":["chat"],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}"#
        )
    };
    Manifest::from_json(&format!(
        r#"{{"default_chat":"chat-default","models":[{},{},{}]}}"#,
        model("chat-default", "default.gguf"),
        model("chat-small", "small.gguf"),
        model("chat-absent", "absent.gguf"),
    ))
    .unwrap()
}

struct Rig {
    _dir: tempfile::TempDir,
    server: FakeModelServer,
    chat: AppClient,
    shell: Client,
}

/// The engine, with the create-time model check on (`check`) or off.
async fn rig(check: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    for file in ["default.gguf", "small.gguf"] {
        std::fs::write(models.join(file), [0u8; 16]).unwrap();
    }
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
        TokenKey::fixed([3; 32]),
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
        gateway: check.then(|| Arc::clone(&gateway)),
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
    };
    let hardware = Arc::new(FixedHardware::with_total(16 << 30));
    let system = StackSystem::new(
        Arc::new(supervisor::Stack::new(None)),
        None,
        hardware,
        Profile::Standard,
    );
    let bound = broker::bind(&paths.engine_socket()).unwrap();
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
        icon: "message-circle".into(),
        capabilities: Capabilities {
            agents: Some(AgentsAccess {}),
            models: Some(ModelAccess {
                any: true,
                ..Default::default()
            }),
            ..Default::default()
        },
        schemas: vec![],
    };
    let chat = shell.scoped(shell.grant(manifest).await.unwrap());
    Rig {
        _dir: dir,
        server,
        chat,
        shell,
    }
}

fn create(model: &str) -> CreateSession {
    CreateSession {
        preset: serde_json::from_value(serde_json::json!("chat")).unwrap(),
        title: None,
        location_opt_in: false,
        model: Some(model.to_string()),
    }
}

/// Send a prompt and return the turn's `turn.end` event.
async fn turn(rig: &Rig, session: &isonomic_proto::agents::SessionId) -> serde_json::Value {
    let mut stream = rig.chat.agents().subscribe(session, 0).await.unwrap();
    rig.chat
        .agents()
        .send_text(session, "Say hello")
        .await
        .unwrap();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
        if let SessionUpdate::Event(e) = next.expect("no update within 10 s").expect("stream ended")
        {
            let value = serde_json::to_value(&*e).unwrap();
            if value["type"] == "turn.end" {
                return value;
            }
        }
    }
}

/// The model of the last reply request the model server got (titles are
/// asked for separately and may follow, so the reply is found by its words).
fn asked_model(rig: &Rig) -> String {
    let asked = rig.server.requests_to("POST /v1/chat/completions");
    let reply = asked
        .iter()
        .rev()
        .find(|r| r.body.contains("Say hello") && !r.body.contains("title"))
        .or(asked.first())
        .expect("a request");
    let body: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    body["model"].as_str().unwrap().to_string()
}

fn code<T>(result: Result<T, ClientError>) -> ErrorCode {
    match result {
        Err(ClientError::Engine(e)) => e.code,
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("expected a refusal"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picked_model_is_checked_then_runs_the_session() {
    let rig = rig(true).await;
    let agents = rig.chat.agents();
    assert_eq!(
        code(agents.create_with(create("nope")).await),
        ErrorCode::UnknownModel
    );
    assert_eq!(
        code(agents.create_with(create("chat-absent")).await),
        ErrorCode::ModelUnavailable
    );
    let long = "m".repeat(agents::MAX_MODEL_NAME + 1);
    assert_eq!(
        code(agents.create_with(create(&long)).await),
        ErrorCode::BadRequest
    );

    let picked = agents.create_with(create("  chat-small  ")).await.unwrap();
    let end = turn(&rig, &picked).await;
    assert_eq!(end["stop"], "done", "{end}");
    assert_eq!(asked_model(&rig), "chat-small");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blank_name_is_refused_and_no_name_runs_the_presets_model() {
    let rig = rig(true).await;
    // As the SDK checks a name: blank is a mistake, not "the preset's own".
    let blank = rig.chat.agents().create_with(create("   ")).await;
    assert_eq!(code(blank), ErrorCode::BadRequest);
    let session = rig.chat.agents().create("chat").await.unwrap();
    let end = turn(&rig, &session).await;
    assert_eq!(end["stop"], "done", "{end}");
    assert_eq!(asked_model(&rig), "chat-default");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_that_slips_through_fails_its_turn_with_a_reason() {
    // No create-time check: the host takes any name, as a registry row
    // from an older catalogue would.
    let rig = rig(false).await;
    let session = rig
        .chat
        .agents()
        .create_with(create("ghost"))
        .await
        .unwrap();
    let end = turn(&rig, &session).await;
    let text = end.to_string();
    assert_ne!(end["stop"], "done", "{end}");
    assert!(text.contains("ghost is not available"), "{text}");
    assert!(rig
        .server
        .requests_to("POST /v1/chat/completions")
        .is_empty());
}

/// A preset the app stores, replying on `model` with titles on `title`.
fn stored_preset(id: &str, model: &str, title: &str) -> String {
    format!(
        "id = \"{id}\"\nversion = 1\nname = \"Stored\"\nclass = \"interactive\"\n\n\
         [persona]\nprompt = \"Be brief.\"\n\n\
         [model]\nid = \"{model}\"\nmax_output_tokens = 256\nreasoning = \"off\"\n\
         title_model = \"{title}\"\n\n\
         [limits]\ntokens = 20000\nsteps = 8\nwall_ms = 60000\n"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn every_model_an_apps_own_preset_names_follows_its_manifest() {
    let rig = rig(true).await;
    let manifest = isonomic_proto::Manifest {
        id: "com.isonomic.notes".into(),
        name: "Notes".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            agents: Some(AgentsAccess {}),
            models: Some(ModelAccess {
                ids: vec!["chat-default".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        schemas: vec![],
    };
    let notes = rig.shell.scoped(rig.shell.grant(manifest).await.unwrap());
    let agents = notes.agents();
    let put = |id: &'static str, model: &'static str, title: &'static str| {
        let agents = notes.agents();
        async move {
            agents
                .put_preset(&stored_preset(id, model, title))
                .await
                .unwrap()
        }
    };

    // Its title model is one the app may not use: refused, before any
    // lookup, whether or not the session names an allowed reply model.
    let titled = put("titled", "chat-default", "chat-small").await;
    let titled = titled.id.to_string();
    assert_eq!(
        code(agents.create(&titled).await),
        ErrorCode::CapabilityDenied
    );
    let named = CreateSession {
        preset: titled.as_str().into(),
        title: None,
        location_opt_in: false,
        model: Some("chat-default".into()),
    };
    assert_eq!(
        code(agents.create_with(named).await),
        ErrorCode::CapabilityDenied
    );
    // A title model the app may not know of at all says no more than that.
    let ghost = put("ghost", "chat-default", "no-such-model")
        .await
        .id
        .to_string();
    assert_eq!(
        code(agents.create(&ghost).await),
        ErrorCode::CapabilityDenied
    );

    // The engine's own choices and allowed models pass.
    let allowed = put("allowed", "local:default", "chat-default")
        .await
        .id
        .to_string();
    agents.create(&allowed).await.unwrap();
    let own = put("own", "chat-default", "local:smallest")
        .await
        .id
        .to_string();
    agents.create(&own).await.unwrap();
}
