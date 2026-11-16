//! Chat turns on a real model, run by the engine's gateway and agents host.
//! Not part of the hermetic suite: it needs a llama.cpp server and a model.
//!
//!     ISONOMIC_TEST_LLAMA_SERVER=<llama-server> ISONOMIC_TEST_MODEL=<model.gguf> \
//!         cargo test -p agents --test real_model -- --ignored --nocapture
//!
//! `ISONOMIC_TEST_PARALLEL` sets the worker's slots (the engine's default
//! otherwise). Each test prints what it measured.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agents::{deps::EngineLocale, EngineScopes, GatewayModel, Parts, SqliteSessionStore};
use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, RamDenial, RamGate, SystemClock};
use gateway::Gateway;
use isonomic_client::{unix_connector, AppClient, Client, Options, SessionStream, SessionUpdate};
use isonomic_model_library::Manifest;
use isonomic_proto::agents::SessionId;
use isonomic_proto::manifest::AgentsAccess;
use isonomic_proto::Capabilities;
use registry::GrantStore;

struct Open;
impl RamGate for Open {
    fn admit(&self, _: u64) -> Result<(), RamDenial> {
        Ok(())
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    chat: AppClient,
}

async fn rig() -> Rig {
    let server = PathBuf::from(std::env::var_os("ISONOMIC_TEST_LLAMA_SERVER").expect("server"));
    let model = PathBuf::from(std::env::var_os("ISONOMIC_TEST_MODEL").expect("model"));
    let parallel: Option<u32> = std::env::var("ISONOMIC_TEST_PARALLEL")
        .ok()
        .and_then(|p| p.parse().ok());
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let file = model.file_name().unwrap().to_string_lossy().to_string();
    std::fs::hard_link(&model, models.join(&file))
        .or_else(|_| std::fs::copy(&model, models.join(&file)).map(|_| ()))
        .unwrap();
    let size = std::fs::metadata(&model).unwrap().len();
    let hash = "0".repeat(64);
    // The catalogue id from the file name, as the model library names it
    // (Qwen3.5-2B-Q4_K_M.gguf is qwen3.5-2b-q4-k-m).
    let id = file
        .trim_end_matches(".gguf")
        .to_lowercase()
        .replace('_', "-");
    let catalogue = Manifest::from_json(&format!(
        r#"{{"default_chat":"{id}","models":[{{"id":"{id}","name":"{id}","file_name":"{file}","source_url":"https://example.invalid/{file}","sha256":"{hash}","size_bytes":{size},"context_length":262144,"capabilities":["chat"],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}]}}"#
    ))
    .unwrap();
    let mut gateway = Gateway::new(catalogue, models, Some(server), Arc::new(Open), true);
    if let Some(parallel) = parallel {
        gateway = gateway.with_configure(Box::new(move |config| {
            config.parallel = parallel;
            config.context_size = 8192 * parallel;
        }));
    }
    let gateway = Arc::new(gateway);
    let apps = Arc::new(Apps::new(
        GrantStore::open(&paths.registry_db()).unwrap(),
        TokenKey::fixed([1; 32]),
    ));
    let host = agents::start(Parts {
        store: Arc::new(SqliteSessionStore::open(&paths.registry_db()).unwrap()),
        scopes: Arc::new(EngineScopes::new(Arc::clone(&apps), Arc::new(SystemClock))),
        model: Arc::new(GatewayModel::new(Arc::clone(&gateway))),
        locale: EngineLocale {
            zone: "Europe/Lisbon".into(),
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
    let hardware = Arc::new(FixedHardware::with_total(16 << 30));
    let system = StackSystem::new(
        Arc::new(supervisor::Stack::new(None)),
        None,
        hardware,
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
        icon: "message-circle".into(),
        capabilities: Capabilities {
            agents: Some(AgentsAccess {}),
            ..Default::default()
        },
        schemas: vec![],
    };
    let chat = shell.scoped(shell.grant(manifest).await.unwrap());
    Rig { _dir: dir, chat }
}

/// The text of the turn's replies, its `turn.end` stop, and when its first
/// text came. With `stop_at_first`, the turn is stopped on its first text.
async fn turn(
    chat: &AppClient,
    session: &SessionId,
    stream: &mut SessionStream,
    started: Instant,
    stop_at_first: bool,
) -> (String, String, Option<Duration>) {
    let mut text = String::new();
    let mut first = None;
    loop {
        let next = tokio::time::timeout(Duration::from_secs(600), stream.next()).await;
        match next.expect("no update within 600 s").expect("stream ended") {
            SessionUpdate::Delta(d) => {
                if first.is_none() {
                    first = Some(started.elapsed());
                    if stop_at_first {
                        chat.agents().cancel(session).await.unwrap();
                    }
                }
                let _ = d;
            }
            SessionUpdate::Event(e) => {
                let value = serde_json::to_value(&*e).unwrap();
                match value["type"].as_str() {
                    Some("assistant") => {
                        first.get_or_insert_with(|| started.elapsed());
                        for part in value["content"].as_array().into_iter().flatten() {
                            text.push_str(part["text"].as_str().unwrap_or(""));
                        }
                    }
                    Some("turn.end") => return (text, value["stop"].to_string(), first),
                    _ => {}
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_LLAMA_SERVER and ISONOMIC_TEST_MODEL"]
async fn a_reply_stopped_on_its_first_token_continues() {
    let rig = rig().await;
    let agents = rig.chat.agents();
    let session = agents.create("chat").await.unwrap();
    let mut stream = agents.subscribe(&session, 0).await.unwrap();
    agents
        .send_text(
            &session,
            std::env::var("ISONOMIC_TEST_PROMPT")
                .as_deref()
                .unwrap_or("Plan one day in Lisbon."),
        )
        .await
        .unwrap();
    let (kept, stop, _) = turn(&rig.chat, &session, &mut stream, Instant::now(), true).await;
    println!(
        "stopped: {stop}, kept {} characters: {kept:?}",
        kept.chars().count()
    );

    let started = Instant::now();
    agents.continue_reply(&session).await.unwrap();
    let (more, stop, first) = turn(&rig.chat, &session, &mut stream, started, false).await;
    println!(
        "continued: {stop}, first text {:?}, {} characters: {:?}",
        first,
        more.chars().count(),
        more.chars().take(120).collect::<String>()
    );
    // A reply that was already whole ("7") gets nothing more.
    assert_eq!(stop, "\"done\"", "the continued turn ended {stop}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_LLAMA_SERVER and ISONOMIC_TEST_MODEL"]
async fn a_reply_stopped_before_its_first_token_continues() {
    let rig = rig().await;
    let agents = rig.chat.agents();
    let session = agents.create("chat").await.unwrap();
    let mut stream = agents.subscribe(&session, 0).await.unwrap();
    agents.send_text(&session, "Say hi.").await.unwrap();
    agents.cancel(&session).await.unwrap();
    let (kept, stop, _) = turn(&rig.chat, &session, &mut stream, Instant::now(), false).await;
    println!(
        "stopped: {stop}, kept {} characters: {kept:?}",
        kept.chars().count()
    );
    let started = Instant::now();
    let continued = agents.continue_reply(&session).await;
    println!("continue: {continued:?}");
    if continued.is_ok() {
        let (more, stop, first) = turn(&rig.chat, &session, &mut stream, started, false).await;
        println!("continued: {stop}, first text {first:?}, {more:?}");
        assert_eq!(stop, "\"done\"", "the continued turn ended {stop}");
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_LLAMA_SERVER and ISONOMIC_TEST_MODEL"]
async fn later_turns_reuse_the_conversations_cache() {
    let rig = rig().await;
    let agents = rig.chat.agents();
    let session = agents.create("chat").await.unwrap();
    let mut stream = agents.subscribe(&session, 0).await.unwrap();
    let prompts = [
        "Plan one day in Lisbon, with a heading for the morning, the afternoon and the evening.",
        "Now plan a second day there, the same way.",
        "And a third day, outside the city.",
    ];
    for (n, prompt) in prompts.iter().enumerate() {
        let started = Instant::now();
        agents.send_text(&session, prompt).await.unwrap();
        let (text, stop, first) = turn(&rig.chat, &session, &mut stream, started, false).await;
        println!(
            "turn {}: first text {:.2} s, total {:.2} s, {} characters, {stop}",
            n + 1,
            first.map_or(0.0, |f| f.as_secs_f64()),
            started.elapsed().as_secs_f64(),
            text.chars().count()
        );
        if n == 0 {
            wait_for_title(&mut stream).await;
        }
    }
}

/// Wait for the title step, as a person reading the first reply would.
async fn wait_for_title(stream: &mut SessionStream) {
    let started = Instant::now();
    loop {
        match tokio::time::timeout(Duration::from_secs(120), stream.next()).await {
            Ok(Some(SessionUpdate::Event(e))) => {
                let value = serde_json::to_value(&*e).unwrap();
                if value["type"] == "title" {
                    println!(
                        "title after {:.2} s: {}",
                        started.elapsed().as_secs_f64(),
                        value["title"]
                    );
                    return;
                }
            }
            Ok(Some(_)) => {}
            _ => {
                println!("no title");
                return;
            }
        }
    }
}

/// The same first request in fresh sessions, as the refusal and length
/// checks ask it: each reply's first text, total time, streamed chunks
/// (about one token each) and text. `ISONOMIC_TEST_RUNS` sets how many
/// (6), `ISONOMIC_TEST_OUT` a file for the replies.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_LLAMA_SERVER and ISONOMIC_TEST_MODEL"]
async fn the_same_first_request_in_fresh_sessions() {
    let rig = rig().await;
    let agents = rig.chat.agents();
    let prompt = std::env::var("ISONOMIC_TEST_PROMPT")
        .unwrap_or_else(|_| "Plan one day in Lisbon.".to_string());
    let runs: usize = std::env::var("ISONOMIC_TEST_RUNS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(6);
    let mut out = String::new();
    for run in 1..=runs {
        let session = agents.create("chat").await.unwrap();
        let mut stream = agents.subscribe(&session, 0).await.unwrap();
        let started = Instant::now();
        agents.send_text(&session, &prompt).await.unwrap();
        let (mut chunks, mut first, mut text) = (0usize, None, String::new());
        let stop = loop {
            let next = tokio::time::timeout(Duration::from_secs(600), stream.next()).await;
            match next.expect("no update within 600 s").expect("stream ended") {
                SessionUpdate::Delta(_) => {
                    chunks += 1;
                    first.get_or_insert_with(|| started.elapsed());
                }
                SessionUpdate::Event(e) => {
                    let value = serde_json::to_value(&*e).unwrap();
                    match value["type"].as_str() {
                        Some("assistant") => {
                            for part in value["content"].as_array().into_iter().flatten() {
                                text.push_str(part["text"].as_str().unwrap_or(""));
                            }
                        }
                        Some("turn.end") => break value["stop"].to_string(),
                        _ => {}
                    }
                }
                other => panic!("unexpected {other:?}"),
            }
        };
        println!(
            "run {run}: first text {:.2} s, total {:.2} s, {chunks} tokens, {} characters, {stop}",
            first.map_or(0.0, |f| f.as_secs_f64()),
            started.elapsed().as_secs_f64(),
            text.chars().count()
        );
        out.push_str(&format!("===== run {run} ({chunks} tokens)\n{text}\n\n"));
    }
    if let Ok(path) = std::env::var("ISONOMIC_TEST_OUT") {
        std::fs::write(path, out).unwrap();
    }
}
