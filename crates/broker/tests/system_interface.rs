//! The `system` interface over a real Unix socket. The first tests use a fake
//! engine; the last drives the real supervisor with a stand-in service and is
//! the engine half of the Milestone 0 gate.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use broker::{BindError, StackSystem, System};
use capsule_core::{FixedHardware, Profile, Tier};
use isonomic_proto::io::{read_message, write_message};
use isonomic_proto::{
    Call, ClientMessage, ErrorCode, Event, HardwareProfile, Outcome, Reply, ServerMessage,
    ServiceState, ServiceStatus, SystemStatus,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::broadcast;

struct Fake {
    status: Mutex<SystemStatus>,
    changes: broadcast::Sender<ServiceStatus>,
    stopped: AtomicBool,
}

fn service(state: ServiceState, restarts: u32) -> ServiceStatus {
    ServiceStatus {
        name: "postgres".into(),
        tier: 1,
        state,
        restart_count: restarts,
        last_healthy_unix: Some(1_700_000_000),
    }
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(SystemStatus {
                aggregate: ServiceState::Healthy,
                services: vec![service(ServiceState::Healthy, 0)],
                cold_start_ms: Some(120),
            }),
            changes: broadcast::channel(16).0,
            stopped: AtomicBool::new(false),
        })
    }
}

impl System for Fake {
    fn status(&self) -> SystemStatus {
        self.status.lock().unwrap().clone()
    }
    fn changes(&self) -> broadcast::Receiver<ServiceStatus> {
        self.changes.subscribe()
    }
    fn logs(&self, service: &str, limit: usize) -> Option<Vec<String>> {
        let all = ["one", "two", "three"].map(String::from);
        (service == "postgres").then(|| all[all.len().saturating_sub(limit)..].to_vec())
    }
    fn profile(&self) -> HardwareProfile {
        HardwareProfile {
            profile: "standard".into(),
            total_memory_bytes: 16 << 30,
            available_memory_bytes: 8 << 30,
        }
    }
    fn request_shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

struct Client(UnixStream, u64);

impl Client {
    async fn connect(socket: &Path) -> Self {
        Self(UnixStream::connect(socket).await.unwrap(), 0)
    }
    async fn greeted(socket: &Path) -> Self {
        let mut client = Self::connect(socket).await;
        let hello = ClientMessage::Hello {
            versions: vec![0],
            client: "test".into(),
        };
        write_message(&mut client.0, &hello).await.unwrap();
        assert!(matches!(
            client.next().await,
            ServerMessage::Welcome { version: 0, .. }
        ));
        client
    }
    async fn next(&mut self) -> ServerMessage {
        let read = read_message::<_, ServerMessage>(&mut self.0);
        tokio::time::timeout(Duration::from_secs(10), read)
            .await
            .expect("no message within 10 s")
            .unwrap()
            .expect("connection closed")
    }
    async fn call(&mut self, call: Call) -> Outcome {
        self.1 += 1;
        let request = ClientMessage::Request {
            id: self.1,
            token: None,
            call,
        };
        write_message(&mut self.0, &request).await.unwrap();
        match self.next().await {
            ServerMessage::Response { id, outcome } if id == self.1 => outcome,
            other => panic!("expected response {}, got {other:?}", self.1),
        }
    }
    async fn event(&mut self) -> ServiceStatus {
        match self.next().await {
            ServerMessage::Event {
                event: Event::ServiceStatus(status),
                ..
            } => status,
            other => panic!("expected an event, got {other:?}"),
        }
    }
}

async fn serve(system: Arc<dyn System>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run/engine.sock");
    let bound = broker::bind(&socket).unwrap();
    let services = broker::Services {
        apps: Arc::new(broker::Apps::new(
            registry::GrantStore::in_memory().unwrap(),
            broker::TokenKey::fixed([7; 32]),
        )),
        blobs: broker::Blobs::open(&dir.path().join("blobs"), &dir.path().join("registry.db"))
            .unwrap(),
        events: broker::Events::default(),
        sql: broker::Sql::new(Arc::new(appdb::StandIn::default())),
        agents: Arc::new(broker::NoAgents),
        models: Arc::new(broker::NoModels),
        identity: Arc::new(broker::identity::NoIdentity),
    };
    tokio::spawn(broker::serve(bound, system, Arc::new(services)));
    (dir, socket)
}

#[tokio::test(flavor = "multi_thread")]
async fn negotiates_then_answers_status_profile_and_logs() {
    let (_dir, socket) = serve(Fake::new()).await;
    let mut client = Client::greeted(&socket).await;

    let Outcome::Result(Reply::Status(status)) = client.call(Call::SystemStatus).await else {
        panic!()
    };
    assert_eq!(status.aggregate, ServiceState::Healthy);
    assert_eq!(status.cold_start_ms, Some(120));

    let Outcome::Result(Reply::Profile(profile)) = client.call(Call::SystemProfile).await else {
        panic!()
    };
    assert_eq!(profile.profile, "standard");

    let logs = Call::SystemLogs {
        service: "postgres".into(),
        limit: Some(2),
    };
    let Outcome::Result(Reply::Logs { lines, .. }) = client.call(logs).await else {
        panic!()
    };
    assert_eq!(lines, ["two", "three"]);

    let missing = Call::SystemLogs {
        service: "nope".into(),
        limit: None,
    };
    let Outcome::Error(error) = client.call(missing).await else {
        panic!()
    };
    assert_eq!(error.code, ErrorCode::UnknownService);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_requests_before_hello_unknown_versions_and_garbage() {
    let (_dir, socket) = serve(Fake::new()).await;

    let mut early = Client::connect(&socket).await;
    let Outcome::Error(error) = early.call(Call::SystemStatus).await else {
        panic!()
    };
    assert_eq!(error.code, ErrorCode::HelloRequired);

    let mut future = Client::connect(&socket).await;
    let hello = ClientMessage::Hello {
        versions: vec![99],
        client: "test".into(),
    };
    write_message(&mut future.0, &hello).await.unwrap();
    let ServerMessage::Response {
        outcome: Outcome::Error(error),
        ..
    } = future.next().await
    else {
        panic!()
    };
    assert_eq!(error.code, ErrorCode::UnsupportedVersion);
    assert_eq!(
        future.0.read(&mut [0u8; 8]).await.unwrap(),
        0,
        "connection should close"
    );

    // A frame that is not a message gets an error, and the connection lives on.
    let mut sloppy = Client::greeted(&socket).await;
    sloppy.0.write_all(&[0, 0, 0, 5]).await.unwrap();
    sloppy.0.write_all(b"{nope").await.unwrap();
    let ServerMessage::Response {
        outcome: Outcome::Error(error),
        ..
    } = sloppy.next().await
    else {
        panic!()
    };
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(matches!(
        sloppy.call(Call::SystemStatus).await,
        Outcome::Result(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_clients_unknown_method_is_answered_with_its_own_id() {
    let (_dir, socket) = serve(Fake::new()).await;
    let mut client = Client::greeted(&socket).await;
    let body = br#"{"type":"request","id":41,"method":"system.teleport","params":{}}"#;
    client
        .0
        .write_all(&(body.len() as u32).to_be_bytes())
        .await
        .unwrap();
    client.0.write_all(body).await.unwrap();
    let ServerMessage::Response {
        id: 41,
        outcome: Outcome::Error(error),
    } = client.next().await
    else {
        panic!("expected an error for id 41")
    };
    assert_eq!(error.code, ErrorCode::UnknownMethod);
    assert!(matches!(
        client.call(Call::SystemStatus).await,
        Outcome::Result(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscription_gives_the_snapshot_then_every_change_until_unsubscribed() {
    let fake = Fake::new();
    let (_dir, socket) = serve(fake.clone()).await;
    let mut client = Client::greeted(&socket).await;

    let Outcome::Result(Reply::Subscribed {
        subscription,
        snapshot,
    }) = client.call(Call::SystemSubscribe).await
    else {
        panic!()
    };
    assert_eq!(snapshot.services[0].state, ServiceState::Healthy);

    fake.changes
        .send(service(ServiceState::Starting, 1))
        .unwrap();
    fake.changes
        .send(service(ServiceState::Healthy, 1))
        .unwrap();
    assert_eq!(client.event().await.state, ServiceState::Starting);
    let recovered = client.event().await;
    assert_eq!(
        (recovered.state, recovered.restart_count),
        (ServiceState::Healthy, 1)
    );

    let stop = Call::SystemUnsubscribe { subscription };
    assert!(matches!(
        client.call(stop).await,
        Outcome::Result(Reply::Unsubscribed {})
    ));
    let again = Call::SystemUnsubscribe { subscription };
    let Outcome::Error(error) = client.call(again).await else {
        panic!()
    };
    assert_eq!(error.code, ErrorCode::UnknownSubscription);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_is_acknowledged_then_passed_to_the_engine() {
    let fake = Fake::new();
    let (_dir, socket) = serve(fake.clone()).await;
    let mut client = Client::greeted(&socket).await;
    assert!(matches!(
        client.call(Call::SystemShutdown).await,
        Outcome::Result(Reply::ShuttingDown {})
    ));
    for _ in 0..200 {
        if fake.stopped.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the engine was never asked to shut down");
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_is_user_only_replaces_a_stale_socket_and_refuses_a_live_one() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("engine.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap()); // leaves a dead file
    let live = broker::bind(&socket).unwrap();
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    // A second engine is refused for as long as the first holds the lock.
    assert!(matches!(
        broker::bind(&socket),
        Err(BindError::AlreadyRunning(_))
    ));
    assert!(
        UnixStream::connect(&socket).await.is_ok(),
        "the first engine's socket was disturbed"
    );
    drop(live);
}

/// Stand-in service plus a fake protocol server for its readiness probe.
async fn stand_in(dir: &Path) -> supervisor::ServiceConfig {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("fake-service");
    std::fs::write(&bin, "#!/bin/sh\necho started\nexec sleep 3600\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let _ = sock.read(&mut [0u8; 64]).await;
                let _ = sock.write_all(b"+PONG\r\n").await;
            });
        }
    });
    let mut config = supervisor::ServiceConfig::new("postgres", bin);
    config.readiness = supervisor::Readiness::RedisPing;
    config.port = Some(port);
    config.probe_interval = Duration::from_millis(25);
    config.steady_probe_interval = Duration::from_secs(3600);
    config.backoff = |_| Duration::from_millis(200);
    config.stop_grace = Duration::from_secs(2);
    config
}

#[tokio::test(flavor = "multi_thread")]
async fn a_force_killed_service_is_seen_restarting_then_healthy_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(registry::ServiceRegistry::in_memory().unwrap());
    let mut stack = supervisor::Stack::new(Some(Arc::clone(&registry)));
    let supervised = stack.add("postgres", Tier::T1, stand_in(dir.path()).await);
    let stack = Arc::new(stack);
    let hardware = Arc::new(FixedHardware::with_total(16 << 30));
    let system = StackSystem::new(
        Arc::clone(&stack),
        Some(registry),
        hardware,
        Profile::Standard,
    );
    system.set_cold_start(stack.start_tier_0_1().await.unwrap());
    let (_socket_dir, socket) = serve(system.clone()).await;

    let mut client = Client::greeted(&socket).await;
    let Outcome::Result(Reply::Subscribed { snapshot, .. }) =
        client.call(Call::SystemSubscribe).await
    else {
        panic!()
    };
    assert_eq!(snapshot.aggregate, ServiceState::Healthy);
    assert!(snapshot.cold_start_ms.is_some());
    assert_eq!(
        (
            snapshot.services[0].tier,
            snapshot.services[0].restart_count
        ),
        (1, 0)
    );

    let pid = supervised.pid().await.unwrap();
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();

    assert_eq!(client.event().await.state, ServiceState::Starting);
    let recovered = client.event().await;
    assert_eq!(recovered.state, ServiceState::Healthy);
    assert_eq!(recovered.restart_count, 1);
    assert!(recovered.last_healthy_unix.is_some());

    assert!(matches!(
        client.call(Call::SystemShutdown).await,
        Outcome::Result(Reply::ShuttingDown {})
    ));
    tokio::time::timeout(Duration::from_secs(5), system.shutdown_requested())
        .await
        .expect("shutdown was not requested");
    stack.shutdown().await;
    assert_eq!(system.status().aggregate, ServiceState::Stopped);
}
