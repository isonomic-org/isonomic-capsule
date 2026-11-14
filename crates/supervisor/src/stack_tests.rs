//! Tests for `stack.rs`: parallel start of a tier, the aggregate status
//! light, and shutdown in reverse dependency order. Services are shell
//! stand-ins behind a fake protocol server.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::probe::Readiness;

/// Stand-in service that records its own name when asked to stop. It drops a
/// `ready-<name>` file once its signal handler is installed: the fake
/// protocol server answers before the script has run a single line.
fn recorder(dir: &Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let body = format!(
        "#!/bin/sh\ntrap 'echo {name} >> {0}/order; exit 0' TERM\ntouch {0}/ready-{name}\nwhile :; do sleep 0.05; done\n",
        dir.display()
    );
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

async fn pong_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 64];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(b"+PONG\r\n").await;
            });
        }
    });
    port
}

async fn config(dir: &Path, name: &str) -> ServiceConfig {
    let mut config = ServiceConfig::new(name, recorder(dir, name));
    config.readiness = Readiness::RedisPing;
    config.port = Some(pong_port().await);
    config.start_timeout = Duration::from_secs(10);
    config.probe_interval = Duration::from_millis(25);
    config.steady_probe_interval = Duration::from_secs(3600);
    config.stop_grace = Duration::from_secs(5);
    config
}

#[tokio::test(flavor = "multi_thread")]
async fn starts_core_tiers_and_stops_in_reverse_dependency_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut stack = Stack::new(None);
    assert_eq!(stack.aggregate_state(), ServiceState::Stopped);
    stack.add("first", Tier::T1, config(dir.path(), "first").await);
    stack.add("second", Tier::T1, config(dir.path(), "second").await);
    let lazy = stack.add("lazy", Tier::T2, config(dir.path(), "lazy").await);

    stack.start_tier_0_1().await.unwrap();
    assert_eq!(
        stack.states(),
        [
            ("first".to_string(), ServiceState::Healthy),
            ("second".to_string(), ServiceState::Healthy),
            ("lazy".to_string(), ServiceState::Stopped),
        ]
    );
    // One stopped service keeps the light off Healthy until it is started.
    assert_eq!(stack.aggregate_state(), ServiceState::Stopped);
    lazy.start().await.unwrap();
    assert_eq!(stack.aggregate_state(), ServiceState::Healthy);

    for name in ["first", "second", "lazy"] {
        let ready = dir.path().join(format!("ready-{name}"));
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    stack.shutdown().await;
    assert!(stack
        .states()
        .iter()
        .all(|(_, state)| *state == ServiceState::Stopped));
    let order = std::fs::read_to_string(dir.path().join("order")).unwrap();
    assert_eq!(
        order.lines().collect::<Vec<_>>(),
        ["lazy", "second", "first"]
    );
}
