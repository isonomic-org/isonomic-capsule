// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tests for `postgres.rs`. `initdb` is a shell stand-in and the server is a
//! fake speaking the wire protocol on a Unix socket: no real PostgreSQL.

use std::os::unix::fs::PermissionsExt;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::endpoint::Endpoint;

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Stand-in `initdb`: creates the directory after `-D`, records its arguments
/// and one variable from its environment.
const FAKE_INITDB: &str = "#!/bin/sh\nargs=\"$*\"\nwhile [ $# -gt 0 ]; do [ \"$1\" = -D ] && d=\"$2\"; shift; done\nmkdir -p \"$d\" && echo 17 > \"$d/PG_VERSION\"\necho \"$args PGPORT=${PGPORT:-unset}\" >> \"$(dirname \"$d\")/initdb-runs\"\n";

fn layout(root: &Path, initdb: PathBuf) -> PostgresLayout {
    PostgresLayout {
        postgres: PathBuf::from("/bundle/postgres"),
        initdb,
        data_dir: root.join("services/postgres"),
        socket_dir: root.join("run/postgres"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn first_time_setup_runs_once_with_a_clean_environment() {
    std::env::set_var("PGPORT", "9999");
    let dir = tempfile::tempdir().unwrap();
    let layout = layout(dir.path(), script(dir.path(), "initdb", FAKE_INITDB));

    let first = prepare(&layout, Profile::Standard, ChildEnv::minimal())
        .await
        .unwrap();
    assert!(first.initialized_now && !first.unclean_shutdown);
    assert!(is_initialized(&layout.data_dir));
    assert!(!staging_dir(&layout.data_dir).exists());
    let mode = std::fs::metadata(&layout.socket_dir)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);

    let second = prepare(&layout, Profile::Standard, ChildEnv::minimal())
        .await
        .unwrap();
    assert!(!second.initialized_now);

    let runs = std::fs::read_to_string(dir.path().join("services/initdb-runs")).unwrap();
    assert_eq!(runs.lines().count(), 1, "{runs}");
    assert!(runs.contains(&format!("-U {SUPERUSER}")) && runs.contains("--auth-host=reject"));
    assert!(
        runs.contains("PGPORT=unset"),
        "the engine's PGPORT leaked: {runs}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_setup_leaves_nothing_behind_and_reports_why() {
    let dir = tempfile::tempdir().unwrap();
    let failing = "#!/bin/sh\nmkdir -p \"$2\"\necho 'no space left' >&2\nexit 3\n";
    let layout = layout(dir.path(), script(dir.path(), "initdb", failing));

    let error = prepare(&layout, Profile::Standard, ChildEnv::minimal())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no space left"), "{error}");
    assert!(!is_initialized(&layout.data_dir));
    assert!(!staging_dir(&layout.data_dir).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unclean_shutdown_is_detected_and_given_recovery_time() {
    let dir = tempfile::tempdir().unwrap();
    let layout = layout(dir.path(), script(dir.path(), "initdb", FAKE_INITDB));
    let clean = prepare(&layout, Profile::Standard, ChildEnv::minimal())
        .await
        .unwrap();
    assert_eq!(clean.config.start_timeout, START_TIMEOUT);

    std::fs::write(layout.data_dir.join("postmaster.pid"), "4242\n").unwrap();
    let unclean = prepare(&layout, Profile::Standard, ChildEnv::minimal())
        .await
        .unwrap();
    assert!(unclean.unclean_shutdown);
    assert_eq!(unclean.config.start_timeout, RECOVERY_START_TIMEOUT);
}

#[test]
fn the_service_listens_on_a_socket_only_and_stops_fast() {
    let layout = layout(Path::new("/c"), PathBuf::from("/bundle/initdb"));
    let config = service(&layout, Profile::Standard, ChildEnv::minimal(), false);
    assert_eq!(config.name, NAME);
    assert_eq!(
        config.socket,
        Some(PathBuf::from("/c/run/postgres/.s.PGSQL.5432"))
    );
    assert_eq!(config.port, None);
    assert_eq!(config.stop_signal, StopSignal::Int);
    let args = config.args.join(" ");
    assert!(args.contains("-c listen_addresses= "), "{args}");
    assert!(args.contains("-k {socket_dir}") && args.contains("-D {data_dir}"));
    assert!(args.contains("unix_socket_permissions=0700"));
}

#[test]
fn shared_buffers_follow_the_hardware_profile() {
    let layout = layout(Path::new("/c"), PathBuf::from("/bundle/initdb"));
    for (profile, buffers) in [
        (Profile::Low, "128MB"),
        (Profile::Standard, "512MB"),
        (Profile::High, "1GB"),
    ] {
        let args = service(&layout, profile, ChildEnv::minimal(), false).args;
        let wanted = format!("shared_buffers={buffers}");
        let at = args.iter().position(|a| *a == wanted);
        assert!(
            at.is_some_and(|i| args[i - 1] == "-c"),
            "{profile}: {args:?}"
        );
    }
}

fn backend_msg(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut msg = vec![kind];
    msg.extend_from_slice(&((4 + body.len()) as i32).to_be_bytes());
    msg.extend_from_slice(body);
    msg
}

#[tokio::test(flavor = "multi_thread")]
async fn the_readiness_probe_speaks_the_wire_protocol_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_path(dir.path());
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                let _ = sock.read(&mut buf).await; // startup
                let mut out = backend_msg(b'R', &0i32.to_be_bytes()); // AuthenticationOk
                out.extend_from_slice(&backend_msg(b'Z', b"I"));
                let _ = sock.write_all(&out).await;
                let _ = sock.read(&mut buf).await; // SELECT 1
                let mut done = backend_msg(b'C', b"SELECT 1\0");
                done.extend_from_slice(&backend_msg(b'Z', b"I"));
                let _ = sock.write_all(&done).await;
            });
        }
    });

    let layout = layout(Path::new("/c"), PathBuf::from("/bundle/initdb"));
    let probe = service(&layout, Profile::Standard, ChildEnv::minimal(), false).readiness;
    assert!(
        probe
            .check(&Endpoint::Unix(socket), Duration::from_secs(2))
            .await
    );
    let nothing = Endpoint::Unix(dir.path().join("absent.sock"));
    assert!(!probe.check(&nothing, Duration::from_millis(300)).await);
}
