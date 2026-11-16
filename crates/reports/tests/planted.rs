//! Planted content never reaches a report: a prompt, a statement's value, a
//! home path and a token, printed by stand-in services that then fail the
//! way real ones do, through the real supervisors. Hermetic: stand-in
//! scripts, a fake protocol server, the model worker on a manual clock.
#![cfg(unix)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use isonomic_inference_core::{FixedGate, ManualClock};
use isonomic_inference_testkit::{wait_for_sleepers, wait_until, write_model, FakeModelServer};
use isonomic_llama::{LlamaConfig, LlamaSupervisor};
use reports::store::Store;
use reports::{Build, Reporter, System};
use serde_json::Value;
use supervisor::{Readiness, ServiceConfig, ServiceState, ServiceSupervisor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PROMPT: &str = "Plan one day in Lisbon for my sister";
const VALUE: &str = "Lisbon with my sister";
const TOKEN: &str = "iso_tok_7f3a9c21b04e5d6f8a1b2c3d4e5f6a7b";

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn reporter(capsule: &Path, home: &str) -> Arc<Reporter> {
    Arc::new(Reporter::new(
        Store::in_capsule(capsule),
        Build::default(),
        System::default(),
        Some(home.to_string()),
    ))
}

/// Every report written, read back.
fn reports(capsule: &Path) -> Vec<Value> {
    Store::in_capsule(capsule)
        .pending()
        .iter()
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect()
}

fn assert_clean(doc: &Value, home: &str) {
    let text = doc.to_string();
    for planted in ["Lisbon", "sister", TOKEN, home] {
        assert!(!text.contains(planted), "{planted:?} in {text}");
    }
}

/// A protocol server that answers +PONG, for the probe.
async fn pong() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 64];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(b"+PONG\r\n").await;
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_database_is_reported_without_what_was_stored() {
    let capsule = tempfile::tempdir().unwrap();
    let home = capsule.path().join("home").to_str().unwrap().to_string();
    let body = format!(
        "#!/bin/sh
echo '2026-11-16 10:00:00.123 UTC [812] LOG:  database system is ready to accept connections' >&2
echo '2026-11-16 10:00:01.004 UTC [907] ERROR:  invalid input syntax for type uuid: \"{VALUE}\"' >&2
echo '2026-11-16 10:00:01.004 UTC [907] STATEMENT:  insert into trips (title, note)' >&2
echo \"\tvalues ('{VALUE}', '{PROMPT}')\" >&2
echo '2026-11-16 10:00:01.005 UTC [907] FATAL:  could not open file {home}/Capsule/services/postgres/base/1: token {TOKEN}' >&2
sleep 0.1
exit 1
"
    );
    let bin = script(capsule.path(), "stand-in-postgres", &body);
    let addr = pong().await;
    let mut config = ServiceConfig::new("postgres", bin);
    config.readiness = Readiness::RedisPing;
    config.port = Some(addr.port());
    config.start_timeout = Duration::from_secs(10);
    config.probe_interval = Duration::from_millis(25);
    config.steady_probe_interval = Duration::from_secs(3600);
    config.probe_timeout = Duration::from_millis(500);
    config.backoff = |_| Duration::ZERO;
    config.stop_grace = Duration::from_millis(500);
    let service = Arc::new(ServiceSupervisor::new(config, None));
    let watch = reports::watch::service(
        reporter(capsule.path(), &home),
        Arc::clone(&service),
        "18.4".into(),
    );
    service.start().await.unwrap();
    let mut rx = service.subscribe();
    tokio::time::timeout(
        Duration::from_secs(30),
        rx.wait_for(|s| *s == ServiceState::Degraded),
    )
    .await
    .expect("the breaker trips")
    .unwrap();
    wait_until("the report", || !reports(capsule.path()).is_empty()).await;
    watch.abort();

    let docs = reports(capsule.path());
    assert_eq!(docs.len(), 1, "one report per trip");
    let doc = &docs[0];
    assert_eq!(doc["kind"], "service_failed");
    assert_eq!(doc["component"]["id"], "engine/postgres");
    assert_eq!(doc["service"]["version"], "18.4");
    assert_eq!(doc["service"]["last_exit"], "exit code 1");
    let tail: Vec<&str> = doc["log_tail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(tail.contains(&"LOG: database system is ready to accept connections"));
    assert!(tail.contains(&"ERROR: invalid input syntax for type uuid: \"[text removed]\""));
    assert!(tail
        .iter()
        .any(|l| l.starts_with("FATAL: could not open file ~/Capsule/")
            && l.contains("[key removed]")));
    assert!(tail
        .iter()
        .all(|l| !l.contains("STATEMENT") && !l.contains("values")));
    assert_clean(doc, &home);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_model_server_is_reported_without_the_prompt() {
    let capsule = tempfile::tempdir().unwrap();
    let home = capsule.path().join("home").to_str().unwrap().to_string();
    let server = FakeModelServer::start().await;
    let clock = Arc::new(ManualClock::new());
    let body = format!(
        "#!/bin/sh
echo 'main: server is listening on http://127.0.0.1:41000' >&2
echo 'slot update_slots: id  0 | task 3 | prompt: {PROMPT}' >&2
echo 'srv  log_server_r: request: POST /v1/chat/completions {TOKEN}' >&2
echo 'error: failed to load model at {home}/Capsule/models/x.gguf' >&2
exit 1
"
    );
    let bin = script(capsule.path(), "stand-in-model-server", &body);
    let mut config = LlamaConfig::new(bin, write_model(capsule.path()));
    config.port = Some(server.port());
    config.idle_unload = None;
    config.backoff = |_| Duration::from_secs(1);
    let worker = Arc::new(LlamaSupervisor::with_clock(
        config,
        Arc::new(FixedGate::unlimited()),
        clock.clone(),
    ));
    let watch = reports::watch::model_server(
        reporter(capsule.path(), &home),
        "qwen3.5-4b".into(),
        Arc::clone(&worker),
        "llama-server".into(),
        "b12387".into(),
    );
    worker.ensure_running().await.unwrap();
    let crashes = |w: &LlamaSupervisor| {
        w.last_logs()
            .iter()
            .filter(|l| l.contains("exited unexpectedly"))
            .count()
    };
    for crash in 1..=5 {
        wait_until("the crash", || crashes(&worker) == crash).await;
        wait_for_sleepers(&clock, 2).await;
        clock.advance(Duration::from_secs(1));
    }
    let mut rx = worker.subscribe();
    tokio::time::timeout(
        Duration::from_secs(30),
        rx.wait_for(|s| *s == isonomic_llama::ServiceState::Degraded),
    )
    .await
    .expect("the breaker trips")
    .unwrap();
    wait_until("both reports", || reports(capsule.path()).len() == 2).await;
    watch.abort();

    let docs = reports(capsule.path());
    let messages: Vec<&str> = docs
        .iter()
        .map(|d| d["message"].as_str().unwrap())
        .collect();
    assert!(
        messages[0].contains("quit unexpectedly and was restarted"),
        "{messages:?}"
    );
    assert!(
        messages[1].contains("was stopped after 5 restarts"),
        "{messages:?}"
    );
    for doc in &docs {
        assert_eq!(doc["kind"], "model_server_failed");
        assert_eq!(doc["component"]["id"], "engine/llama-server");
        assert_eq!(doc["model"]["id"], "qwen3.5-4b");
        assert_eq!(doc["model"]["last_exit"], "exit code 1");
        let tail = doc["log_tail"].to_string();
        assert!(tail.contains("main: server is listening"), "{tail}");
        assert!(
            tail.contains("error: failed to load model at ~/Capsule/models/x.gguf"),
            "{tail}"
        );
        assert!(
            !tail.contains("slot ") && !tail.contains("request:"),
            "{tail}"
        );
        assert_clean(doc, &home);
    }
}
