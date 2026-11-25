// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tests for `service.rs`, included from it via `#[path]` so they reach the
//! engine's private items. Stand-in binaries are shell scripts; the protocol
//! server is a fake. Nothing here needs a real service.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::env::SYSTEM_PATH;
use crate::spawn::resolve_args;

fn write_script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Stand-in protocol server: answers +PONG to everything.
async fn spawn_pong_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
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
    (addr, handle)
}

fn test_config(
    binary: PathBuf,
    port: u16,
    registry: bool,
) -> (ServiceConfig, Option<Arc<ServiceRegistry>>) {
    let mut config = ServiceConfig::new("cache", binary);
    config.args = vec!["--port".to_string(), "{port}".to_string()];
    config.readiness = Readiness::RedisPing;
    config.port = Some(port);
    config.start_timeout = Duration::from_secs(10);
    config.probe_interval = Duration::from_millis(25);
    config.steady_probe_interval = Duration::from_secs(3600);
    config.probe_timeout = Duration::from_millis(500);
    config.backoff = |_| Duration::ZERO;
    // Production grace is 15 s; tests must never wait that long.
    config.stop_grace = Duration::from_millis(500);
    let registry = registry.then(|| Arc::new(ServiceRegistry::in_memory().unwrap()));
    (config, registry)
}

async fn wait_for_state(sup: &ServiceSupervisor, want: ServiceState, within: Duration) {
    let mut rx = sup.subscribe();
    let result = tokio::time::timeout(within, rx.wait_for(|s| *s == want)).await;
    match result {
        Ok(Ok(_)) => {}
        _ => panic!(
            "did not reach {want:?} in {within:?}; state is {:?}",
            sup.state()
        ),
    }
}

async fn wait_until_gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn starts_probes_and_records_registry_state() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_script(dir.path(), "fake-service", "#!/bin/sh\nexec sleep 3600\n");
    let (addr, _pong) = spawn_pong_server().await;
    let (config, registry) = test_config(bin, addr.port(), true);
    let registry = registry.unwrap();
    let sup = ServiceSupervisor::new(config, Some(Arc::clone(&registry)));

    assert_eq!(sup.state(), ServiceState::Stopped);
    let got = sup.start().await.unwrap();
    assert_eq!(got.port(), Some(addr.port()));
    assert_eq!(sup.state(), ServiceState::Healthy);
    assert_eq!(sup.name(), "cache");

    let record = registry.get("cache").unwrap().unwrap();
    assert_eq!(record.port, Some(addr.port()));
    assert_eq!(record.state, "healthy");
    assert_eq!(record.restart_count, 0);
    assert!(record.last_healthy_unix.is_some());
    assert!(!sup.last_logs().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_9_restarts_and_registry_counts_it() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_script(dir.path(), "fake-service", "#!/bin/sh\nexec sleep 3600\n");
    let (addr, _pong) = spawn_pong_server().await;
    let (config, registry) = test_config(bin, addr.port(), true);
    let registry = registry.unwrap();
    let sup = ServiceSupervisor::new(config, Some(Arc::clone(&registry)));
    sup.start().await.unwrap();
    let pid = sup.pid().await.unwrap();
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while sup.restart_total() == 0 {
        assert!(Instant::now() < deadline, "no restart observed");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    wait_for_state(&sup, ServiceState::Healthy, Duration::from_secs(10)).await;
    assert_ne!(sup.pid().await.unwrap(), pid);
    assert!(registry.get("cache").unwrap().unwrap().restart_count >= 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn breaker_trips_to_degraded_and_registry_reflects_it() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_script(
        dir.path(),
        "flaky-service",
        "#!/bin/sh\necho starting\nsleep 0.2\nexit 1\n",
    );
    let (addr, _pong) = spawn_pong_server().await;
    let (config, registry) = test_config(bin, addr.port(), true);
    let registry = registry.unwrap();
    let sup = ServiceSupervisor::new(config, Some(Arc::clone(&registry)));
    sup.start().await.unwrap();
    wait_for_state(&sup, ServiceState::Degraded, Duration::from_secs(30)).await;
    assert!(!sup.last_logs().is_empty());
    assert_eq!(registry.get("cache").unwrap().unwrap().state, "degraded");
    assert!(matches!(
        sup.start().await.unwrap_err(),
        ServiceError::Degraded(_)
    ));
    sup.reset().await;
    assert_eq!(sup.state(), ServiceState::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_supervisor_tears_down_the_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_script(dir.path(), "fake-service", "#!/bin/sh\nexec sleep 3600\n");
    let (addr, _pong) = spawn_pong_server().await;
    let (config, _) = test_config(bin, addr.port(), false);
    let sup = ServiceSupervisor::new(config, None);
    sup.start().await.unwrap();
    let pid = sup.pid().await.unwrap();
    drop(sup);
    assert!(
        wait_until_gone(pid, Duration::from_secs(5)).await,
        "child survived supervisor drop"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_land_in_a_file_when_logs_dir_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_script(
        dir.path(),
        "fake-service",
        "#!/bin/sh\necho hello-from-service\nexec sleep 3600\n",
    );
    let (addr, _pong) = spawn_pong_server().await;
    let (mut config, _) = test_config(bin, addr.port(), false);
    config.logs_dir = Some(dir.path().join("logs"));
    let sup = ServiceSupervisor::new(config, None);
    sup.start().await.unwrap();
    // Wait for the line itself, not for a fixed time. macOS can hold the
    // first run of a brand-new script for many seconds while it assesses it.
    let log = dir.path().join("logs/cache.log");
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut content = String::new();
    while Instant::now() < deadline {
        content = std::fs::read_to_string(&log).unwrap_or_default();
        if content.contains("hello-from-service") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        content.contains("hello-from-service"),
        "log file: {content}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn config_file_is_rendered_before_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_script(dir.path(), "fake-service", "#!/bin/sh\nexec sleep 3600\n");
    let (addr, _pong) = spawn_pong_server().await;
    let (mut config, _) = test_config(bin, addr.port(), false);
    let conf_path = dir.path().join("rendered/cache.conf");
    config = config.with_config_file(conf_path.clone(), "port {port}\nbind {host}\n");
    let sup = ServiceSupervisor::new(config, None);
    sup.start().await.unwrap();
    let rendered = std::fs::read_to_string(&conf_path).unwrap();
    assert!(
        rendered.contains(&format!("port {}", addr.port())),
        "{rendered}"
    );
    assert!(rendered.contains("bind 127.0.0.1"), "{rendered}");
    assert!(!rendered.contains('{'), "unsubstituted: {rendered}");
}

/// `StopSignal::Int` is the `pg_ctl stop -m fast` equivalent: a service that
/// ignores SIGTERM but handles SIGINT must still exit promptly, well inside the
/// grace period — which is only possible if SIGINT is what we sent.
#[tokio::test(flavor = "multi_thread")]
async fn stop_signal_int_reaches_a_service_that_ignores_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_script(
        dir.path(),
        "ignores-term",
        "#!/bin/sh\ntrap '' TERM\ntrap 'exit 0' INT\nwhile true; do sleep 0.05; done\n",
    );
    let (addr, _pong) = spawn_pong_server().await;
    let (mut config, _) = test_config(bin, addr.port(), false);
    config.stop_signal = StopSignal::Int;
    config.stop_grace = Duration::from_secs(5);
    let sup = ServiceSupervisor::new(config, None);
    sup.start().await.unwrap();
    let pid = sup.pid().await.unwrap();

    let started = Instant::now();
    sup.stop().await;
    let elapsed = started.elapsed();

    assert!(
        wait_until_gone(pid, Duration::from_secs(2)).await,
        "survived stop"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "stop took {elapsed:?} — looks like it waited out the grace instead of being handled"
    );
    assert_eq!(sup.state(), ServiceState::Stopped);
}

/// A service that ignores every catchable signal is killed once the grace
/// period expires — "15 s deadline each, then kill", with the deadline made
/// short here so the test is fast.
#[tokio::test(flavor = "multi_thread")]
async fn a_service_that_ignores_signals_is_killed_after_the_grace() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_script(
        dir.path(),
        "ignores-everything",
        "#!/bin/sh\ntrap '' TERM\ntrap '' INT\nwhile true; do sleep 0.05; done\n",
    );
    let (addr, _pong) = spawn_pong_server().await;
    let (mut config, _) = test_config(bin, addr.port(), false);
    config.stop_grace = Duration::from_millis(300);
    let sup = ServiceSupervisor::new(config, None);
    sup.start().await.unwrap();
    let pid = sup.pid().await.unwrap();

    sup.stop().await;

    assert!(
        wait_until_gone(pid, Duration::from_secs(5)).await,
        "child survived the stop deadline"
    );
    assert_eq!(sup.state(), ServiceState::Stopped);
}

#[test]
fn templates_substitute_for_ports_and_for_sockets() {
    let mut config = ServiceConfig::new("svc", PathBuf::from("/bin/svc"));
    config.data_dir = Some(PathBuf::from("/tmp/svc-data"));
    config.args = ["{port}", "{host}", "{data_dir}", "{socket}", "{socket_dir}"]
        .map(String::from)
        .to_vec();
    let tcp = Endpoint::Tcp("127.0.0.1:40125".parse().unwrap());
    assert_eq!(
        resolve_args(&config, &tcp),
        ["40125", "127.0.0.1", "/tmp/svc-data", "", ""]
    );
    let unix = Endpoint::Unix(PathBuf::from("/c/run/svc/s.sock"));
    assert_eq!(
        resolve_args(&config, &unix),
        ["", "", "/tmp/svc-data", "/c/run/svc/s.sock", "/c/run/svc"]
    );
}

#[test]
fn defaults_are_sigterm_and_the_fifteen_second_grace() {
    let config = ServiceConfig::new("anything", PathBuf::from("/bin/true"));
    assert_eq!(config.stop_signal, StopSignal::Term);
    assert_eq!(config.stop_grace, DEFAULT_STOP_GRACE);
    assert_eq!(DEFAULT_STOP_GRACE, Duration::from_secs(15));
}

/// The allow-list holds: a child sees only declared variables, whatever the
/// engine itself inherited.
#[tokio::test(flavor = "multi_thread")]
async fn a_child_sees_only_allow_listed_variables() {
    // The kind of variables that redirected or broke services before.
    std::env::set_var("PGPORT", "1");
    std::env::set_var("DYLD_LIBRARY_PATH", "/nowhere");
    std::env::set_var("ISONOMIC_TEST_LEAK", "leaked");

    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("env.txt");
    let body = format!(
        "#!/bin/sh\nenv > {0}.tmp && mv {0}.tmp {0}\nexec sleep 3600\n",
        dump.display()
    );
    let bin = write_script(dir.path(), "fake-service", &body);
    let (addr, _pong) = spawn_pong_server().await;
    let (mut config, _) = test_config(bin, addr.port(), false);
    config.env = ChildEnv::minimal().with("SERVICE_ROLE", "primary");
    let sup = ServiceSupervisor::new(config, None);
    sup.start().await.unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    while !dump.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let seen = std::fs::read_to_string(&dump).unwrap();
    let get = |name: &str| {
        seen.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .map(str::to_string)
    };
    for leaked in [
        "PGPORT",
        "DYLD_LIBRARY_PATH",
        "ISONOMIC_TEST_LEAK",
        "HOME",
        "USER",
    ] {
        assert_eq!(get(leaked), None, "{leaked} reached the child:\n{seen}");
    }
    assert_eq!(get("PATH").as_deref(), Some(SYSTEM_PATH));
    assert_eq!(get("SERVICE_ROLE").as_deref(), Some("primary"));
    sup.stop().await;
}

/// Set only for the stand-in below, so `--ignored` runs don't sit in it.
const STAND_IN: &str = "ISONOMIC_TEST_STAND_IN";
/// Arguments that make a copy of this test program run only the stand-in.
const STAND_IN_ARGS: [&str; 3] = ["--ignored", "--exact", "service::tests::stand_in_service"];

/// A long-running program for the orphan tests, reached by copying this test
/// binary under the service's name. A script would show up as its shell, and
/// a copy of a system tool may be one multi-call program that only works
/// under its own name.
#[test]
#[ignore]
fn stand_in_service() {
    if std::env::var_os(STAND_IN).is_some() {
        std::thread::sleep(Duration::from_secs(30));
    }
}

fn stand_in(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::copy(std::env::current_exe().unwrap(), &path).unwrap();
    path
}

fn stand_in_config(program: PathBuf, port: u16, pid_file: PathBuf) -> ServiceConfig {
    let (mut config, _) = test_config(program, port, false);
    config.args = STAND_IN_ARGS.iter().map(|a| a.to_string()).collect();
    config.env = ChildEnv::minimal().with(STAND_IN, "1");
    config.pid_file = Some(pid_file);
    config
}

/// Start `program` the way an earlier engine did (its own process group)
/// and reap it when it exits, so a stopped one doesn't linger as a zombie.
fn earlier_engines(program: &std::path::Path) -> u32 {
    use std::os::unix::process::CommandExt;
    let mut child = std::process::Command::new(program)
        .args(STAND_IN_ARGS)
        .env(STAND_IN, "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    pid
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_left_running_by_an_earlier_engine_is_stopped_first() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, _server) = spawn_pong_server().await;
    let program = stand_in(dir.path(), "cache-server");
    let old = earlier_engines(&program);
    let pid_file = dir.path().join("run").join("cache.pid");
    std::fs::create_dir_all(pid_file.parent().unwrap()).unwrap();
    std::fs::write(&pid_file, format!("{old}\n")).unwrap();
    assert!(
        !wait_until_gone(old, Duration::from_millis(200)).await,
        "it runs"
    );

    let config = stand_in_config(program, addr.port(), pid_file.clone());
    let sup = ServiceSupervisor::new(config, None);
    sup.start().await.unwrap();

    assert!(wait_until_gone(old, Duration::from_secs(5)).await);
    assert!(sup
        .last_logs()
        .iter()
        .any(|line| line.contains(&format!("left running by an earlier engine (pid {old})"))));
    let new = sup.pid().await.unwrap();
    assert_ne!(new, old);
    let noted = std::fs::read_to_string(&pid_file).unwrap();
    assert_eq!(noted.trim(), new.to_string());

    sup.stop().await;
    assert!(!pid_file.exists(), "a deliberate stop leaves no record");
}

#[tokio::test(flavor = "multi_thread")]
async fn with_its_pid_file_gone_a_left_service_is_found_by_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, _server) = spawn_pong_server().await;
    let program = stand_in(dir.path(), "postgres");
    let old = earlier_engines(&program);
    // The run directory was cleaned; the service's own file remains.
    let own = dir.path().join("postmaster.pid");
    std::fs::write(&own, format!("{old}\n/the/data/dir\n")).unwrap();
    let mut config = stand_in_config(program, addr.port(), dir.path().join("run/postgres.pid"));
    config.own_pid_file = Some(own);
    let sup = ServiceSupervisor::new(config, None);
    sup.start().await.unwrap();
    assert!(
        wait_until_gone(old, Duration::from_secs(5)).await,
        "stopped first"
    );
    assert_ne!(sup.pid().await.unwrap(), old);
    sup.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reused_pid_running_something_else_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, _server) = spawn_pong_server().await;
    let stranger = earlier_engines(&stand_in(dir.path(), "someone-else"));
    let pid_file = dir.path().join("cache.pid");
    std::fs::write(&pid_file, format!("{stranger}\n")).unwrap();
    assert!(
        !wait_until_gone(stranger, Duration::from_millis(200)).await,
        "it runs"
    );

    let config = stand_in_config(stand_in(dir.path(), "cache-server"), addr.port(), pid_file);
    let sup = ServiceSupervisor::new(config, None);
    sup.start().await.unwrap();

    assert!(!wait_until_gone(stranger, Duration::from_millis(300)).await);
    sup.stop().await;
    unsafe {
        libc::kill(stranger as libc::pid_t, libc::SIGKILL);
    }
}
