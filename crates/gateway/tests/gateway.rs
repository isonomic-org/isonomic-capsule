//! The `models` interface over a real socket: the gateway with a fake model
//! server in place of llama.cpp and a stand-in worker program, under a
//! scripted memory gate. Hermetic: no model, no download, no real server.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, RamDenial, RamGate};
use gateway::Gateway;
use isonomic_client::{unix_connector, AppClient, Client, ClientError, Options};
use isonomic_inference_testkit::{write_script, FakeModelServer};
use isonomic_model_library::Manifest;
use isonomic_proto::manifest::ModelAccess;
use isonomic_proto::models::{Message, ModelState, StopReason, StreamRequest};
use isonomic_proto::{Capabilities, ErrorCode};
use registry::GrantStore;

/// Records its arguments beside itself, then waits like a server.
const SLEEPER: &str = "#!/bin/sh\necho \"$@\" >> \"$0.args\"\nexec /bin/sleep 3600\n";

fn catalogue_json() -> String {
    let hash = "0".repeat(64);
    let model = |id: &str, file: &str, size: u64, caps: &str| {
        format!(
            r#"{{"id":"{id}","name":"{id}","file_name":"{file}","source_url":"https://example.invalid/{file}","sha256":"{hash}","size_bytes":{size},"context_length":4096,"capabilities":[{caps}],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}"#
        )
    };
    format!(
        r#"{{"default_chat":"chat-big","default_embeddings":"embed","models":[{},{},{},{}]}}"#,
        model("chat-big", "big.gguf", 32, r#""chat""#),
        model("chat-small", "small.gguf", 16, r#""chat""#),
        model("embed", "embed.gguf", 8, r#""embeddings""#),
        model("absent", "absent.gguf", 8, r#""chat""#),
    )
}

/// Admits up to a limit a test can lower.
struct Gate(AtomicU64);

impl RamGate for Gate {
    fn admit(&self, bytes: u64) -> Result<(), RamDenial> {
        let available = self.0.load(Ordering::SeqCst);
        if bytes <= available {
            return Ok(());
        }
        Err(RamDenial {
            required_bytes: bytes,
            available_bytes: available,
            reason: "not enough free memory to load the model".into(),
        })
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    server: FakeModelServer,
    shell: Client,
    gate: Arc<Gate>,
    program: std::path::PathBuf,
}

fn write(dir: &Path, name: &str, size: usize) {
    std::fs::write(dir.join(name), vec![0u8; size]).unwrap();
}

async fn rig(slow: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    write(&models, "big.gguf", 32);
    write(&models, "small.gguf", 16);
    write(&models, "embed.gguf", 8);
    let server = FakeModelServer::start().await;
    let port = server.port();
    let program = write_script(dir.path(), "stand-in", SLEEPER);
    let gate = Arc::new(Gate(AtomicU64::new(1 << 30)));
    let manifest = Manifest::from_json(&catalogue_json()).unwrap();
    let gateway = Gateway::new(manifest, models, Some(program.clone()), gate.clone(), slow)
        .with_configure(Box::new(move |config| {
            config.port = Some(port);
            config.idle_unload = None;
            config.probe_interval = Duration::from_millis(20);
        }));
    let services = Services {
        apps: Arc::new(Apps::new(
            GrantStore::in_memory().unwrap(),
            TokenKey::fixed([4; 32]),
        )),
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(Arc::new(appdb::StandIn::default())),
        agents: Arc::new(broker::NoAgents),
        models: Arc::new(gateway),
        identity: Arc::new(broker::identity::NoIdentity),
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
    Rig {
        _dir: dir,
        server,
        shell,
        gate,
        program,
    }
}

async fn app(rig: &Rig, ids: &[&str]) -> AppClient {
    let manifest = isonomic_proto::Manifest {
        id: "com.isonomic.chat".into(),
        name: "Chat".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            models: Some(ModelAccess {
                any: ids.is_empty(),
                ids: ids.iter().map(|s| s.to_string()).collect(),
                remote: false,
            }),
            ..Default::default()
        },
        schemas: vec![],
    };
    rig.shell.scoped(rig.shell.grant(manifest).await.unwrap())
}

fn hello(model: Option<&str>) -> StreamRequest {
    StreamRequest {
        model: model.map(str::to_string),
        messages: vec![Message::user("hi")],
        ..Default::default()
    }
}

fn code<T>(result: Result<T, ClientError>) -> ErrorCode {
    match result {
        Err(ClientError::Engine(e)) => e.code,
        Err(other) => panic!("expected an engine error, got {other:?}"),
        Ok(_) => panic!("expected an engine error, got a result"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_runs_on_the_default_model_and_ends_with_one_stop() {
    let rig = rig(false).await;
    let chat = app(&rig, &[]).await;
    let mut stream = chat.models().stream(hello(None)).await.unwrap();
    assert_eq!(stream.model(), "chat-big", "the manifest's default");
    let mut text = String::new();
    let mut stops = Vec::new();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("no chunk within 10 s")
    {
        let chunk = chunk.unwrap();
        text.push_str(&chunk.text);
        stops.extend(chunk.stop);
    }
    assert_eq!(text, "Hello world");
    assert_eq!(stops, [StopReason::End]);
    let info = chat.models().info("chat-big").await.unwrap();
    assert_eq!(info.state, ModelState::Ready);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_machine_defaults_to_the_smallest_chat_model() {
    let rig = rig(true).await;
    let chat = app(&rig, &[]).await;
    let stream = chat.models().stream(hello(None)).await.unwrap();
    assert_eq!(stream.model(), "chat-small");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_list_is_installed_models_the_app_may_use() {
    let rig = rig(false).await;
    let chat = app(&rig, &["chat-small", "absent"]).await;
    let listed: Vec<_> = chat
        .models()
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(listed, ["chat-small"]);
    let all = rig.shell.models().list().await.unwrap();
    // The picker's mark: the model `models.recommended` names, and only it.
    let marked: Vec<_> = all
        .iter()
        .filter(|m| m.recommended)
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(marked, ["chat-big"]);
    assert!(
        rig.shell
            .models()
            .info("chat-big")
            .await
            .unwrap()
            .info
            .recommended
    );
    assert!(
        !rig.shell
            .models()
            .info("chat-small")
            .await
            .unwrap()
            .info
            .recommended
    );
    assert_eq!(all.len(), 3);
    assert_eq!(
        code(chat.models().stream(hello(Some("chat-big"))).await),
        ErrorCode::CapabilityDenied
    );
    assert_eq!(
        code(chat.models().stream(hello(Some("absent"))).await),
        ErrorCode::ModelUnavailable
    );
    assert_eq!(
        code(chat.models().stream(hello(Some("nope"))).await),
        ErrorCode::CapabilityDenied
    );
    assert_eq!(
        code(rig.shell.models().stream(hello(Some("nope"))).await),
        ErrorCode::UnknownModel
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_memory_gate_refuses_before_a_stream_opens_with_its_numbers() {
    let rig = rig(false).await;
    rig.gate.0.store(20, Ordering::SeqCst);
    let chat = app(&rig, &[]).await;
    match chat.models().stream(hello(Some("chat-big"))).await {
        Err(ClientError::Engine(e)) => {
            assert_eq!(e.code, ErrorCode::InsufficientMemory);
            let memory = e.memory.expect("the gate's numbers");
            assert_eq!((memory.required_bytes, memory.available_bytes), (32, 20));
        }
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("the gate let it through"),
    }
    // A smaller model still fits.
    chat.models()
        .stream(hello(Some("chat-small")))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn embeddings_come_from_the_embedding_model_and_chat_models_refuse() {
    let rig = rig(false).await;
    let chat = app(&rig, &[]).await;
    let vectors = chat.models().embed(None, vec!["a".into()]).await.unwrap();
    assert_eq!(vectors, [vec![0.1, 0.2]]);
    let refused = chat
        .models()
        .embed(Some("chat-small"), vec!["a".into()])
        .await;
    assert_eq!(code(refused), ErrorCode::Unsupported);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_opener_cancels_and_the_stream_ends_cancelled() {
    let rig = rig(false).await;
    rig.server.hold_stream_after_first_event();
    let chat = app(&rig, &[]).await;
    let mut stream = chat.models().stream(hello(None)).await.unwrap();
    let first = stream.next().await.unwrap().unwrap();
    assert!(first.stop.is_none());

    let subscription = stream.subscription();
    let other = rig.shell.scoped(
        rig.shell
            .grant(isonomic_proto::Manifest {
                id: "com.isonomic.notes".into(),
                name: "Notes".into(),
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
            })
            .await
            .unwrap(),
    );
    let theft = other
        .call(isonomic_proto::Call::ModelsCancel { subscription })
        .await;
    assert_eq!(code(theft), ErrorCode::UnknownSubscription);

    stream.cancel().await.unwrap();
    let mut stops = Vec::new();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("no chunk within 10 s")
    {
        stops.extend(chunk.unwrap().stop);
    }
    assert_eq!(stops, [StopReason::Cancelled]);
    rig.server.release_stream();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_worker_has_a_slot_for_titles_and_keeps_each_slots_context() {
    let rig = rig(false).await;
    let chat = app(&rig, &[]).await;
    chat.models().stream(hello(None)).await.unwrap();
    let file = rig.program.with_extension("args");
    // The worker writes it as it starts; a limit, not a wait for a policy.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !file.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let args = std::fs::read_to_string(&file).unwrap();
    let args: Vec<&str> = args.split_whitespace().collect();
    let after = |flag: &str| args.iter().position(|a| *a == flag).map(|i| args[i + 1]);
    assert_eq!(after("-np"), Some("2"), "{args:?}");
    assert_eq!(after("-c"), Some("16384"), "{args:?}");
    // Not a Qwen3.5 model: its own chat template.
    assert_eq!(after("--chat-template-file"), None);
}

#[test]
fn a_worker_unloads_after_the_profiles_idle_time() {
    use capsule_core::Profile;
    use isonomic_llama::ServerMode;
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let manifest = Manifest::from_json(&catalogue_json()).unwrap();
    let entry = manifest.default_chat().clone();
    let gateway = |idle: Option<Option<Duration>>| {
        let manifest = Manifest::from_json(&catalogue_json()).unwrap();
        let gate = Arc::new(Gate(AtomicU64::new(1 << 30)));
        let server = Some(dir.path().join("llama-server"));
        let g = Gateway::new(manifest, models.clone(), server, gate, false);
        match idle {
            Some(idle) => g.with_idle_unload(idle),
            None => g,
        }
    };
    let idle = |g: Gateway, mode| g.launch_config(&entry, mode).unwrap().idle_unload;
    let standard = Profile::Standard.model_idle_unload();
    assert_eq!(standard, Some(Duration::from_secs(15 * 60)));
    assert_eq!(idle(gateway(Some(standard)), ServerMode::Chat), standard);
    assert_eq!(
        idle(gateway(Some(None)), ServerMode::Chat),
        None,
        "high: never"
    );
    let low = Profile::Low.model_idle_unload();
    assert_eq!(idle(gateway(Some(low)), ServerMode::Embedding), low);
    // Without one, the inference supervisor's own default.
    assert_eq!(
        idle(gateway(None), ServerMode::Chat),
        Some(Duration::from_secs(5 * 60))
    );
}
