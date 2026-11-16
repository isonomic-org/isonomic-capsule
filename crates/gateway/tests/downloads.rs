//! Model downloads and the first-model recommendation, over a real socket.
//! The model file comes from a loopback HTTP server in the test, which can
//! hold the transfer halfway; nothing leaves the machine.

use std::sync::Arc;
use std::time::Duration;

use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, RamDenial, RamGate};
use gateway::{Gateway, Machine};
use isonomic_client::{unix_connector, Client, ClientError, DownloadStream, Options};
use isonomic_model_library::Manifest;
use isonomic_proto::models::{DownloadOutcome, DownloadProgress, ModelState};
use isonomic_proto::ErrorCode;
use registry::GrantStore;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;

const BODY: usize = 64 * 1024;

struct Open;
impl RamGate for Open {
    fn admit(&self, _: u64) -> Result<(), RamDenial> {
        Ok(())
    }
}

/// Serves `body` for any GET, from a range's start if it names one. While
/// `hold` is true it sends up to half and waits.
async fn file_server(body: Vec<u8>, hold: watch::Receiver<bool>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let body = body.clone();
            let mut hold = hold.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                // A range continues an earlier attempt.
                let text = String::from_utf8_lossy(&request).to_lowercase();
                let start = text
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|r| r.trim().trim_end_matches('-').parse::<usize>().ok())
                    .filter(|s| *s < body.len())
                    .unwrap_or(0);
                let head = if start == 0 {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                } else {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\
                         Content-Range: bytes {start}-{}/{}\r\nConnection: close\r\n\r\n",
                        body.len() - start,
                        body.len() - 1,
                        body.len()
                    )
                };
                let half = (body.len() / 2).max(start);
                if socket.write_all(head.as_bytes()).await.is_err()
                    || socket.write_all(&body[start..half]).await.is_err()
                {
                    return;
                }
                let _ = socket.flush().await;
                while *hold.borrow_and_update() {
                    if hold.changed().await.is_err() {
                        return;
                    }
                }
                let _ = socket.write_all(&body[half..]).await;
            });
        }
    });
    port
}

fn manifest(hash: &str, size: usize) -> Manifest {
    let entry = |id: &str, file: &str, size: usize, hash: &str| {
        format!(
            r#"{{"id":"{id}","name":"{id}","file_name":"{file}","source_url":"https://example.invalid/{file}","sha256":"{hash}","size_bytes":{size},"context_length":4096,"capabilities":["chat"],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}"#
        )
    };
    let wrong = "f".repeat(64);
    Manifest::from_json(&format!(
        r#"{{"default_chat":"big","models":[{},{},{}]}}"#,
        entry("big", "big.gguf", size, hash),
        entry("small", "small.gguf", 16, &"0".repeat(64)),
        entry("corrupt", "corrupt.gguf", size, &wrong),
    ))
    .unwrap()
}

struct Rig {
    _dir: tempfile::TempDir,
    shell: Client,
    hold: watch::Sender<bool>,
}

async fn rig(total_memory_bytes: u64) -> Rig {
    rig_on(total_memory_bytes, 1 << 40, |_| {}).await
}

/// The rig with `free` bytes on the disk, after `prepare` has seen the
/// model directory.
async fn rig_on(total_memory_bytes: u64, free: u64, prepare: impl FnOnce(&std::path::Path)) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let body: Vec<u8> = (0..BODY).map(|i| (i % 251) as u8).collect();
    let hash: String = Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let (hold, held) = watch::channel(false);
    let port = file_server(body, held).await;
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("small.gguf"), [0u8; 16]).unwrap();
    prepare(&models);
    let gateway = Gateway::new(manifest(&hash, BODY), models, None, Arc::new(Open), false)
        .with_disk(Arc::new(gateway::FixedDisk(free)))
        .with_source(Box::new(move |entry| {
            let mut spec = entry.spec();
            spec.url = format!("http://127.0.0.1:{port}/{}", spec.file_name);
            spec
        }))
        .with_machine(Machine {
            profile: "standard".into(),
            total_memory_bytes,
        });
    let services = Services {
        apps: Arc::new(Apps::new(
            GrantStore::in_memory().unwrap(),
            TokenKey::fixed([2; 32]),
        )),
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(Arc::new(appdb::StandIn::default())),
        agents: Arc::new(broker::NoAgents),
        models: Arc::new(gateway),
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
    Rig {
        _dir: dir,
        shell,
        hold,
    }
}

async fn next(stream: &mut DownloadStream) -> Option<DownloadProgress> {
    let item = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
    item.expect("no progress within 10 s").map(|p| p.unwrap())
}

async fn last(stream: &mut DownloadStream) -> DownloadProgress {
    let mut seen = None;
    while let Some(p) = next(stream).await {
        seen = Some(p);
    }
    seen.expect("at least one event")
}

fn code<T>(result: Result<T, ClientError>) -> ErrorCode {
    match result {
        Err(ClientError::Engine(e)) => e.code,
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("expected a refusal"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_reports_progress_and_installs_the_verified_file() {
    let rig = rig(16 << 30).await;
    let models = rig.shell.models();
    assert_eq!(
        models.info("big").await.unwrap().state,
        ModelState::NotInstalled
    );
    assert_eq!(
        code(models.embed(Some("big"), vec![]).await),
        ErrorCode::ModelUnavailable
    );

    let mut stream = models.download("big").await.unwrap();
    assert_eq!(stream.total_bytes(), Some(BODY as u64));
    let done = last(&mut stream).await;
    assert_eq!(
        done.outcome,
        Some(DownloadOutcome::Installed),
        "{:?}",
        done.error
    );
    assert_eq!(done.received_bytes, BODY as u64);
    assert_eq!(models.info("big").await.unwrap().state, ModelState::Stopped);
    let listed: Vec<_> = models
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert!(listed.contains(&"big".to_string()));

    // Downloading an installed model answers with one final event.
    let mut again = models.download("big").await.unwrap();
    let only = next(&mut again).await.unwrap();
    assert_eq!(only.outcome, Some(DownloadOutcome::Installed));
    assert!(next(&mut again).await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_window_attaches_and_cancel_ends_every_follower() {
    let rig = rig(16 << 30).await;
    rig.hold.send_replace(true);
    let models = rig.shell.models();
    let mut first = models.download("big").await.unwrap();
    let mut halfway = next(&mut first).await.unwrap();
    while halfway.received_bytes < (BODY / 2) as u64 {
        halfway = next(&mut first).await.unwrap();
    }
    assert_eq!(
        models.info("big").await.unwrap().state,
        ModelState::Downloading
    );

    let mut second = models.download("big").await.unwrap();
    let joined = next(&mut second).await.unwrap();
    assert!(joined.received_bytes >= (BODY / 2) as u64, "{joined:?}");
    assert!(joined.outcome.is_none());

    models.cancel_download("big").await.unwrap();
    assert_eq!(
        last(&mut first).await.outcome,
        Some(DownloadOutcome::Cancelled)
    );
    assert_eq!(
        last(&mut second).await.outcome,
        Some(DownloadOutcome::Cancelled)
    );
    assert_eq!(
        code(models.cancel_download("big").await),
        ErrorCode::NotDownloading
    );
    assert_eq!(
        models.info("big").await.unwrap().state,
        ModelState::NotInstalled
    );
    rig.hold.send_replace(false);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_fails_its_checksum_is_not_installed() {
    let rig = rig(16 << 30).await;
    let models = rig.shell.models();
    let mut stream = models.download("corrupt").await.unwrap();
    let done = last(&mut stream).await;
    assert_eq!(done.outcome, Some(DownloadOutcome::Failed));
    assert!(done.error.unwrap().message.contains("checksum"));
    assert_eq!(
        models.info("corrupt").await.unwrap().state,
        ModelState::NotInstalled
    );
    assert_eq!(code(models.download("nope").await), ErrorCode::UnknownModel);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_model_follows_the_profile_and_the_memory_floor() {
    let rig = rig(16 << 30).await;
    let models = rig.shell.models();
    let standard = models.recommended(None).await.unwrap();
    assert_eq!(
        (standard.model.as_str(), standard.profile.as_str()),
        ("big", "standard")
    );
    assert_eq!(standard.size_bytes, Some(BODY as u64));
    let low = models.recommended(Some("low")).await.unwrap();
    assert_eq!(low.model, "small");
    assert_eq!(
        code(models.recommended(Some("huge")).await),
        ErrorCode::BadRequest
    );

    let small = rig_small().await;
    match small.shell.models().recommended(None).await {
        Err(ClientError::Engine(e)) => {
            assert_eq!(e.code, ErrorCode::InsufficientMemory);
            let memory = e.memory.unwrap();
            assert_eq!(memory.available_bytes, 8 << 30, "the machine's total");
            assert_eq!(memory.required_bytes, gateway::MEMORY_FLOOR);
        }
        other => panic!("{:?}", other.map(|r| r.model)),
    }
}

async fn rig_small() -> Rig {
    rig(8 << 30).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_without_room_on_disk_is_refused_in_plain_words() {
    let rig = rig_on(16 << 30, BODY as u64 + gateway::DISK_HEADROOM - 1, |_| {}).await;
    match rig.shell.models().download("big").await {
        Err(ClientError::Engine(e)) => {
            assert_eq!(e.code, ErrorCode::TooLarge);
            assert!(
                e.message
                    .contains("not enough free disk space to download big"),
                "{}",
                e.message
            );
        }
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("the download started without room for it"),
    }
    let models = rig.shell.models();
    assert_eq!(
        models.info("big").await.unwrap().state,
        ModelState::NotInstalled
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_cut_off_by_a_quit_picks_up_its_part_and_installs_whole() {
    let body: Vec<u8> = (0..BODY).map(|i| (i % 251) as u8).collect();
    let half = body[..BODY / 2].to_vec();
    // An earlier engine quit with half the file written. Only the bytes
    // still missing need room on the disk.
    let free = (BODY / 2) as u64 + gateway::DISK_HEADROOM;
    let rig = rig_on(16 << 30, free, move |models| {
        std::fs::write(models.join("big.gguf.part"), half).unwrap();
    })
    .await;
    let models = rig.shell.models();
    assert_eq!(
        models.info("big").await.unwrap().state,
        ModelState::NotInstalled
    );
    let mut stream = models.download("big").await.unwrap();
    let first = next(&mut stream).await.unwrap();
    assert!(first.received_bytes >= (BODY / 2) as u64, "{first:?}");
    let done = last(&mut stream).await;
    assert_eq!(
        done.outcome,
        Some(DownloadOutcome::Installed),
        "{:?}",
        done.error
    );
    assert_eq!(models.info("big").await.unwrap().state, ModelState::Stopped);
}
