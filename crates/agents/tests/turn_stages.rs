//! Where a chat turn's time goes, on a real model, through the engine's own
//! path: the broker, the check on every app call, the agents host, the
//! gateway and the model server, with the engine's chat-worker settings.
//! Not part of the hermetic suite. On the Mac:
//!
//!     ISONOMIC_TEST_LLAMA_SERVER=<llama-server> ISONOMIC_TEST_MODEL=<model.gguf> \
//!         cargo test --release -p agents --test turn_stages -- --ignored --nocapture
//!
//! A scratch Capsule in a temporary folder, its Isonomic ID in the identity
//! library's in-memory store: nothing is written to the keychain. The model
//! file is hard-linked in (copied if it cannot be) and only read. The model
//! server runs in its own process group and is stopped when the test ends.
//!
//! Two chats of two prompts each. In the first, the second prompt is sent
//! as soon as the first reply ends, while the engine may still be writing
//! the chat's title on the model; in the second, after the title. For each
//! prompt it prints, from the moment it was sent:
//!
//! - when the engine received `sessions.send`, and how long its check took;
//! - the model server's own lines as they appear: which slot took the turn,
//!   how many prompt tokens it reused from its cache and how many it
//!   evaluated, and its timings;
//! - the engine's own lines on each title: how long it waited for quiet,
//!   whether it was sent, and whether it yielded to the person;
//! - when the first text reached the app, and when the turn ended.
//!
//! A third chat stops its second prompt at the first text and continues at
//! once, as the self-check does, and prints the slot the continue took and
//! how many prompt tokens it read, with the engine's `reply:` lines.
//!
//! It fails if the second prompt sent at once reaches its first text later
//! than one and a half times the after-title case plus a second: a person's
//! prompt must never wait behind a title. The engine's title rules
//! (`agents::TITLE_QUIET`) are on, as in the engine.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agents::{deps::EngineLocale, EngineScopes, GatewayModel, Parts, SqliteSessionStore};
use broker::agents::BoxFuture;
use broker::{Apps, Blobs, Events, Identity, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, RamDenial, RamGate, SystemClock};
use gateway::Gateway;
use isonomic_client::{unix_connector, AppClient, Client, Options, SessionUpdate};
use isonomic_proto::identity::Identity as PersonIdentity;
use isonomic_proto::manifest::AgentsAccess;
use isonomic_proto::{Call, Capabilities, Manifest, WireError};
use registry::GrantStore;

struct Open;
impl RamGate for Open {
    fn admit(&self, _: u64) -> Result<(), RamDenial> {
        Ok(())
    }
}

/// When things happened, from the latest send.
#[derive(Default)]
struct Clock {
    sent: Option<Instant>,
    marks: Vec<String>,
}

type Shared = Arc<Mutex<Clock>>;

fn mark(clock: &Shared, what: String) {
    let mut c = clock.lock().unwrap();
    let at = c.sent.map_or(0.0, |s| s.elapsed().as_secs_f64());
    c.marks.push(format!("  +{at:7.3} s  {what}"));
}

/// The engine's real check, timed: it runs when a call arrives.
struct Timed {
    inner: identity::IdentityService,
    clock: Shared,
}

impl Identity for Timed {
    fn get(&self) -> BoxFuture<'_, Result<PersonIdentity, WireError>> {
        self.inner.get()
    }
    fn granted<'a>(&'a self, manifest: &'a Manifest) -> BoxFuture<'a, Result<(), WireError>> {
        self.inner.granted(manifest)
    }
    fn check<'a>(
        &'a self,
        manifest: &'a Manifest,
        call: &'a Call,
    ) -> BoxFuture<'a, Result<(), WireError>> {
        Box::pin(async move {
            let started = Instant::now();
            let result = self.inner.check(manifest, call).await;
            if call.method() == "sessions.send" {
                let took = started.elapsed().as_secs_f64() * 1e6;
                mark(
                    &self.clock,
                    format!("engine received sessions.send (check {took:.0} µs)"),
                );
            }
            result
        })
    }
}

/// New lines in `now` after the ring last read as `before`.
fn new_lines(before: &[String], now: &[String]) -> Vec<String> {
    let overlap = (0..=before.len().min(now.len()))
        .rev()
        .find(|&k| before[before.len() - k..] == now[..k])
        .unwrap_or(0);
    now[overlap..].to_vec()
}

/// The model server lines that say where a turn's time went.
fn telling(line: &str) -> bool {
    let text = line
        .trim_start_matches("[stderr] ")
        .trim_start_matches("[stdout] ");
    text.starts_with("slot ")
        || text.contains("prompt eval time")
        || text.contains("eval time")
        || text.contains("total time")
        || text.contains("cache")
}

async fn rig(clock: Shared) -> (tempfile::TempDir, AppClient) {
    let server = PathBuf::from(std::env::var_os("ISONOMIC_TEST_LLAMA_SERVER").expect("server"));
    let model = PathBuf::from(std::env::var_os("ISONOMIC_TEST_MODEL").expect("model"));
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let file = model.file_name().unwrap().to_string_lossy().to_string();
    std::fs::hard_link(&model, models.join(&file))
        .or_else(|_| std::fs::copy(&model, models.join(&file)).map(|_| ()))
        .unwrap();
    let size = std::fs::metadata(&model).unwrap().len();
    let id = file
        .trim_end_matches(".gguf")
        .to_lowercase()
        .replace('_', "-");
    let catalogue = isonomic_model_library::Manifest::from_json(&format!(
        r#"{{"default_chat":"{id}","models":[{{"id":"{id}","name":"{id}","file_name":"{file}","source_url":"https://example.invalid/{file}","sha256":"{}","size_bytes":{size},"context_length":262144,"capabilities":["chat"],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}]}}"#,
        "0".repeat(64)
    ))
    .unwrap();
    // The engine's own word on each title: waited, sent, or yielded.
    {
        let clock = Arc::clone(&clock);
        tokio::spawn(async move {
            let mut seen: Vec<String> = Vec::new();
            loop {
                let now = capsule_core::log::tail();
                for line in new_lines(&seen, &now) {
                    if line.starts_with("title:") || line.starts_with("reply:") {
                        mark(&clock, format!("engine: {line}"));
                    }
                }
                seen = now;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
    }
    // Watch the model server's log, stamping each telling line as it comes.
    let watch = Arc::clone(&clock);
    let gateway = Arc::new(
        Gateway::new(catalogue, models, Some(server), Arc::new(Open), true).with_on_worker(
            Box::new(move |_, worker| {
                let (worker, clock) = (Arc::clone(worker), Arc::clone(&watch));
                tokio::spawn(async move {
                    let mut seen: Vec<String> = Vec::new();
                    loop {
                        let now = worker.last_logs();
                        for line in new_lines(&seen, &now).into_iter().filter(|l| telling(l)) {
                            mark(
                                &clock,
                                format!("model: {}", line.trim_start_matches("[stderr] ")),
                            );
                        }
                        seen = now;
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                });
            }),
        ),
    );
    let apps = Arc::new(Apps::new(
        GrantStore::open(&paths.registry_db()).unwrap(),
        TokenKey::fixed([1; 32]),
    ));
    let host = agents::start(Parts {
        store: Arc::new(SqliteSessionStore::open(&paths.registry_db()).unwrap()),
        scopes: Arc::new(EngineScopes::new(Arc::clone(&apps), Arc::new(SystemClock))),
        model: Arc::new(
            GatewayModel::new(Arc::clone(&gateway)).with_title_quiet(agents::TITLE_QUIET),
        ),
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
    let capsule_id = capsule_core::format::open(&paths, "test")
        .unwrap()
        .capsule_id;
    let person = identity::Person::new(
        dir.path(),
        &capsule_id,
        identity::memory_backend(),
        Arc::new(SystemClock),
    );
    let identity = Timed {
        inner: identity::IdentityService::new(person).with_apps(Arc::clone(&apps)),
        clock,
    };
    let services = Services {
        apps,
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(Arc::new(appdb::StandIn::default())),
        agents: Arc::new(host),
        models: gateway,
        identity: Arc::new(identity),
    };
    let system = StackSystem::new(
        Arc::new(supervisor::Stack::new(None)),
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
    // First run, as the window does it: the ID, then Chat's approval.
    shell.identity().await.unwrap();
    let manifest = Manifest {
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
    (dir, chat)
}

const PROMPTS: [&str; 2] = [
    "Plan one day in Lisbon, with a heading for the morning, the afternoon and the evening.",
    "Now plan a second day there, the same way.",
];

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_LLAMA_SERVER and ISONOMIC_TEST_MODEL"]
async fn where_a_turns_time_goes() {
    let clock: Shared = Arc::default();
    let (_dir, chat) = rig(Arc::clone(&clock)).await;
    let agents = chat.agents();
    // When the second prompt's first text came, in each case.
    let mut second_first: Vec<Duration> = Vec::new();
    for (case, wait_for_title) in [
        ("second prompt at once", false),
        ("second prompt after the title", true),
    ] {
        println!("== {case}");
        let session = agents.create("chat").await.unwrap();
        let mut stream = agents.subscribe(&session, 0).await.unwrap();
        for (n, prompt) in PROMPTS.iter().enumerate() {
            {
                let mut c = clock.lock().unwrap();
                c.sent = Some(Instant::now());
                c.marks.clear();
            }
            mark(&clock, format!("sent prompt {}", n + 1));
            agents.send_text(&session, prompt).await.unwrap();
            let sent_at = Instant::now();
            let mut first = false;
            loop {
                let next = tokio::time::timeout(Duration::from_secs(600), stream.next()).await;
                match next.expect("no update within 600 s").expect("stream ended") {
                    SessionUpdate::Delta(_) if !first => {
                        first = true;
                        if n == 1 {
                            second_first.push(sent_at.elapsed());
                        }
                        mark(&clock, "first text reached the app".into());
                    }
                    SessionUpdate::Event(e) => {
                        let value = serde_json::to_value(&*e).unwrap();
                        match value["type"].as_str() {
                            Some("assistant") if !first => {
                                first = true;
                                if n == 1 {
                                    second_first.push(sent_at.elapsed());
                                }
                                mark(&clock, "first text reached the app".into());
                            }
                            Some("title") => mark(&clock, "title written".into()),
                            Some("turn.end") => {
                                mark(&clock, format!("turn ended: {}", value["stop"]));
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            if n == 0 && wait_for_title {
                let waited = Instant::now();
                let titled = loop {
                    let left = Duration::from_secs(120).saturating_sub(waited.elapsed());
                    match tokio::time::timeout(left, stream.next()).await {
                        Ok(Some(SessionUpdate::Event(e)))
                            if serde_json::to_value(&*e).unwrap()["type"] == "title" =>
                        {
                            break true
                        }
                        Ok(Some(_)) => {}
                        _ => break false,
                    }
                };
                let what = if titled {
                    "title written"
                } else {
                    "no title within 120 s"
                };
                mark(&clock, what.into());
            }
            // Let the model server's last lines in.
            tokio::time::sleep(Duration::from_millis(50)).await;
            for line in clock.lock().unwrap().marks.drain(..) {
                println!("{line}");
            }
        }
    }
    // Stop then continue, as the self-check does: the continue must land on
    // the slot that holds the conversation.
    println!("== stop, then continue at once");
    let session = agents.create("chat").await.unwrap();
    let mut stream = agents.subscribe(&session, 0).await.unwrap();
    let mut continue_first = None;
    for (n, step) in ["prompt 1", "prompt 2, stopped", "continue"]
        .iter()
        .enumerate()
    {
        {
            let mut c = clock.lock().unwrap();
            c.sent = Some(Instant::now());
            c.marks.clear();
        }
        mark(&clock, format!("sent {step}"));
        let sent_at = Instant::now();
        match n {
            2 => {
                if let Err(e) = agents.continue_reply(&session).await {
                    println!("  continue refused: {e:?}");
                    break;
                }
            }
            _ => {
                agents.send_text(&session, PROMPTS[n]).await.unwrap();
            }
        }
        let mut first = false;
        loop {
            let next = tokio::time::timeout(Duration::from_secs(600), stream.next()).await;
            let update = next.expect("no update within 600 s").expect("stream ended");
            let text = match &update {
                SessionUpdate::Delta(_) => true,
                SessionUpdate::Event(e) => {
                    serde_json::to_value(&**e).unwrap()["type"] == "assistant"
                }
                _ => false,
            };
            if text && !first {
                first = true;
                mark(&clock, "first text reached the app".into());
                if n == 1 {
                    agents.cancel(&session).await.unwrap();
                    mark(&clock, "stopped".into());
                }
                if n == 2 {
                    continue_first = Some(sent_at.elapsed());
                }
            }
            if let SessionUpdate::Event(e) = &update {
                let value = serde_json::to_value(&**e).unwrap();
                if value["type"] == "turn.end" {
                    mark(&clock, format!("turn ended: {}", value["stop"]));
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        for line in clock.lock().unwrap().marks.drain(..) {
            println!("{line}");
        }
    }
    if let Some(first) = continue_first {
        println!(
            "continue after a stop: first text {:.2} s",
            first.as_secs_f64()
        );
    }

    // A second prompt sent at once never waits behind the title: it reaches
    // its first text about as soon as one sent after the title does.
    let [at_once, after_title] = second_first[..] else {
        panic!("first text for both second prompts: {second_first:?}")
    };
    let bound = after_title.mul_f64(1.5) + Duration::from_secs(1);
    println!(
        "second prompt first text: at once {:.2} s, after the title {:.2} s (bound {:.2} s)",
        at_once.as_secs_f64(),
        after_title.as_secs_f64(),
        bound.as_secs_f64()
    );
    assert!(
        at_once <= bound,
        "the second prompt waited behind the title"
    );
}
