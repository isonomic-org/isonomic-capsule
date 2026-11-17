// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tests for `stack.rs`: parallel start of a tier, the aggregate status
//! light, shutdown in reverse dependency order, lazy starts and the RAM gate.
//! Services are shell stand-ins behind a fake protocol server.

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

/// Stand-in that stays up until stopped, or exits at once when `fail` is
/// set.
fn stand_in(dir: &Path, name: &str, fail: bool) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let body = if fail {
        "#!/bin/sh\nexit 3\n"
    } else {
        "#!/bin/sh\ntrap 'exit 0' TERM\nwhile :; do sleep 0.05; done\n"
    };
    let path = dir.join(format!("{name}.sh"));
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// How many times the supervisor launched a service, from its own log. The
/// script can't count itself: readiness comes from the fake server, and on a
/// busy Mac a freshly written script may not run its first line before the
/// start is over.
fn launches(stack: &Stack, name: &str) -> usize {
    stack
        .get(name)
        .unwrap()
        .last_logs()
        .iter()
        .filter(|line| line.starts_with("spawned "))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lazy_service_waits_for_first_use_and_concurrent_callers_share_one_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut stack = Stack::new(None);
    stack.add("core", Tier::T1, config(dir.path(), "core").await);
    let mut lazy = config(dir.path(), "lazy").await;
    lazy.binary_path = stand_in(dir.path(), "lazy", false);
    stack.add("lazy", Tier::T2, lazy);
    let stack = Arc::new(stack);

    stack.start_tier_0_1().await.unwrap();
    assert_eq!(
        stack.get("lazy").unwrap().pid().await,
        None,
        "tier 2 is not started at boot"
    );
    assert!(idle_lazy(Tier::T2, stack.get("lazy").unwrap().state()));

    let callers: Vec<_> = (0..5)
        .map(|_| {
            let stack = Arc::clone(&stack);
            tokio::spawn(async move { stack.ensure_started("lazy").await })
        })
        .collect();
    for caller in callers {
        caller.await.unwrap().unwrap();
    }
    assert_eq!(launches(&stack, "lazy"), 1, "one start for every caller");
    stack.ensure_started("lazy").await.unwrap();
    assert_eq!(launches(&stack, "lazy"), 1, "already running");
    assert_eq!(stack.aggregate_state(), ServiceState::Healthy);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_lazy_start_reaches_every_waiter_and_the_next_call_retries() {
    let dir = tempfile::tempdir().unwrap();
    let mut stack = Stack::new(None);
    let mut lazy = config(dir.path(), "broken").await;
    lazy.binary_path = stand_in(dir.path(), "broken", true);
    // A listener that never answers, held for the whole test: readiness can
    // only fail. A port closed again could be taken by another test's fake
    // server, which would answer.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    lazy.port = Some(silent.local_addr().unwrap().port());
    lazy.start_timeout = Duration::from_secs(2);
    stack.add("broken", Tier::T2, lazy);
    let stack = Arc::new(stack);

    let callers: Vec<_> = (0..3)
        .map(|_| {
            let stack = Arc::clone(&stack);
            tokio::spawn(async move { stack.ensure_started("broken").await })
        })
        .collect();
    let mut outcomes = Vec::new();
    for caller in callers {
        outcomes.push(caller.await.unwrap());
    }
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o, Err(LazyError::Failed { .. }))),
        "{outcomes:?}"
    );
    assert_eq!(
        launches(&stack, "broken"),
        1,
        "waiters share the failed attempt"
    );
    assert!(stack.ensure_started("broken").await.is_err());
    assert_eq!(launches(&stack, "broken"), 2, "a later call tries again");
    stack.shutdown().await;
    drop(silent);
}

#[tokio::test]
async fn only_known_tier_two_services_start_lazily() {
    let dir = tempfile::tempdir().unwrap();
    let mut stack = Stack::new(None);
    stack.add("core", Tier::T1, config(dir.path(), "core").await);
    assert_eq!(
        stack.ensure_started("core").await,
        Err(LazyError::NotLazy("core".into()))
    );
    assert_eq!(
        stack.ensure_started("nope").await,
        Err(LazyError::Unknown("nope".into()))
    );
}

fn gate(free_bytes: u64) -> Arc<dyn RamGate> {
    use capsule_core::{FixedHardware, Hardware, Profile, ProfileRamGate};
    let hardware = FixedHardware(Hardware {
        total_memory_bytes: 64 << 30,
        available_memory_bytes: free_bytes,
        pressure: capsule_core::Pressure::Normal,
    });
    Arc::new(ProfileRamGate::new(Profile::High, Arc::new(hardware)).with_headroom(0))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ram_gate_admits_a_lazy_service_that_fits() {
    let dir = tempfile::tempdir().unwrap();
    let mut stack = Stack::new(None).with_gate(gate(8 << 30));
    let mut heavy = config(dir.path(), "heavy").await;
    heavy.binary_path = stand_in(dir.path(), "heavy", false);
    stack.add_gated("heavy", heavy, 4 << 30);

    stack.ensure_started("heavy").await.unwrap();
    assert_eq!(launches(&stack, "heavy"), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_starts_nothing_and_carries_the_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let mut stack = Stack::new(None).with_gate(gate(2 << 30));
    let mut heavy = config(dir.path(), "heavy").await;
    heavy.binary_path = stand_in(dir.path(), "heavy", false);
    stack.add_gated("heavy", heavy, 4 << 30);

    match stack.ensure_started("heavy").await {
        Err(LazyError::Refused { name, denial }) => {
            assert_eq!(name, "heavy");
            assert_eq!(denial.required_bytes, 4 << 30);
            assert_eq!(denial.available_bytes, 2 << 30);
            assert!(
                denial.reason.contains("Not enough free memory"),
                "{}",
                denial.reason
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(
        stack.get("heavy").unwrap().pid().await,
        None,
        "nothing was started"
    );
    assert!(idle_lazy(Tier::T2, stack.get("heavy").unwrap().state()));
}

#[tokio::test]
async fn a_gated_service_without_a_gate_is_not_started_unchecked() {
    let dir = tempfile::tempdir().unwrap();
    let mut stack = Stack::new(None);
    let mut heavy = config(dir.path(), "heavy").await;
    heavy.binary_path = stand_in(dir.path(), "heavy", false);
    stack.add_gated("heavy", heavy, 1);

    let outcome = stack.ensure_started("heavy").await;
    assert!(
        matches!(outcome, Err(LazyError::Failed { .. })),
        "{outcome:?}"
    );
    assert_eq!(stack.get("heavy").unwrap().pid().await, None);
}

#[tokio::test]
async fn an_ungated_lazy_service_does_not_consult_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    // A gate that refuses everything: the vector database declares no
    // footprint, so it starts regardless.
    let mut stack = Stack::new(None).with_gate(gate(0));
    let mut light = config(dir.path(), "light").await;
    light.binary_path = stand_in(dir.path(), "light", false);
    stack.add("light", Tier::T2, light);

    stack.ensure_started("light").await.unwrap();
    assert_eq!(launches(&stack, "light"), 1);
    stack.shutdown().await;
}
