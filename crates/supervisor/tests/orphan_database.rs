//! A real database left running by an engine killed outright. Not part of
//! the hermetic suite: it needs the server binaries and a user other than
//! root, as the database refuses to run as root.
//!
//!     ISONOMIC_TEST_POSTGRES_BIN=<bin dir> cargo test -p supervisor \
//!         --test orphan_database -- --ignored --test-threads=1
//!
//! The first engine is this test program run again as a child: it starts
//! the database through the supervisor, says so, and is killed with
//! SIGKILL, so nothing of it shuts down.
//!
//! - Its guardian then stops the database cleanly, so the next start
//!   needs no crash recovery.
//! - Should the guardian be gone too, the database is left running in its
//!   own process group, holding the data directory. A second engine on the
//!   same Capsule must stop it before it starts its own, so the data
//!   directory never has two servers.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use capsule_core::{CapsulePaths, Profile, Tier};
use supervisor::{postgres, ChildEnv, Stack};

const ROLE: &str = "ISONOMIC_TEST_ORPHAN_ENGINE";

fn layout(root: &Path, bin: &Path) -> postgres::PostgresLayout {
    let paths = CapsulePaths::new(root);
    postgres::PostgresLayout {
        postgres: bin.join("postgres"),
        initdb: bin.join("initdb"),
        data_dir: paths.service_dir(postgres::NAME),
        socket_dir: paths.service_socket_dir(postgres::NAME),
    }
}

/// Start the database as the engine binary does, with its pid file.
async fn engine(root: &Path, bin: &Path) -> Arc<Stack> {
    let paths = CapsulePaths::new(root);
    paths.create_dirs().unwrap();
    let layout = layout(root, bin);
    let mut prepared = postgres::prepare(&layout, Profile::Low, ChildEnv::minimal())
        .await
        .unwrap();
    prepared.config.pid_file = Some(paths.run_dir().join("postgres.pid"));
    let mut stack = Stack::new(None);
    stack.add(postgres::NAME, Tier::T1, prepared.config);
    let stack = Arc::new(stack);
    stack.start_tier_0_1().await.unwrap();
    stack
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// The pid the data directory's lock file names: the server holding it.
fn holder(root: &Path, bin: &Path) -> Option<u32> {
    let lock = layout(root, bin).data_dir.join("postmaster.pid");
    std::fs::read_to_string(lock)
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

/// Kill the guardian watching process group `pgid`, found by the group in
/// its arguments (never by name alone).
fn kill_guardian_of(pgid: u32) {
    let out = Command::new("/bin/ps")
        .args(["-axo", "pid=,args="])
        .output()
        .unwrap();
    let wanted = format!("isonomic-service-guardian {pgid} ");
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim_start();
        let Some((pid, args)) = line.split_once(' ') else {
            continue;
        };
        if args.contains(&wanted) {
            let pid: libc::pid_t = pid.parse().unwrap();
            // SAFETY: a guardian this test's own engine started.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

/// Run the first engine as a child, wait for its database, kill it, and
/// return the database it left behind. With `and_its_guardian`, the
/// database's guardian is killed first, leaving a real orphan.
fn first_engine_dies(root: &Path, and_its_guardian: bool) -> u32 {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "first_engine", "--ignored", "--nocapture"])
        .env(ROLE, root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let pid = loop {
        let line = lines
            .next()
            .expect("the first engine said nothing")
            .unwrap();
        if let Some(pid) = line.strip_prefix("DATABASE ") {
            break pid.trim().parse::<u32>().unwrap();
        }
    };
    if and_its_guardian {
        kill_guardian_of(pid);
    }
    // SAFETY: the child is ours; SIGKILL leaves it no chance to clean up.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGKILL) };
    let _ = child.wait();
    pid
}

fn bin() -> PathBuf {
    PathBuf::from(std::env::var_os("ISONOMIC_TEST_POSTGRES_BIN").expect("bin dir"))
}

/// The first engine's part, run only as `first_engine_dies`'s child.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "run by the orphan tests"]
async fn first_engine() {
    let Some(root) = std::env::var_os(ROLE) else {
        return;
    };
    let stack = engine(Path::new(&root), &bin()).await;
    let pid = stack.get(postgres::NAME).unwrap().pid().await.unwrap();
    println!("DATABASE {pid}");
    std::future::pending::<()>().await;
}

async fn second_engine_takes_over(drop_our_pid_file: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (root, bin) = (dir.path(), bin());
    let old = first_engine_dies(root, true);
    assert!(alive(old), "the killed engine's database is still running");
    assert_eq!(holder(root, &bin), Some(old));
    if drop_our_pid_file {
        let paths = CapsulePaths::new(root);
        std::fs::remove_file(paths.run_dir().join("postgres.pid")).unwrap();
    }

    let stack = engine(root, &bin).await;
    let new = stack.get(postgres::NAME).unwrap().pid().await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(old) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(old), "the old database was stopped");
    assert_ne!(new, old);
    assert_eq!(holder(root, &bin), Some(new), "one server holds the data");
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_POSTGRES_BIN and a non-root user"]
async fn a_killed_engines_guardian_stops_its_database_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let (root, bin) = (dir.path(), bin());
    let old = first_engine_dies(root, false);
    let died = Instant::now();
    // The database's stop grace is the supervisor's; this is a safety net.
    let deadline = died + Duration::from_secs(30);
    while alive(old) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(old), "the guardian stopped the database");
    let data = layout(root, &bin).data_dir;
    assert!(!postgres::had_unclean_shutdown(&data), "a clean shutdown");
    println!(
        "stopped {:.1} s after the engine died",
        died.elapsed().as_secs_f64()
    );
    let stack = engine(root, &bin).await;
    assert_eq!(
        holder(root, &bin),
        stack.get(postgres::NAME).unwrap().pid().await
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_POSTGRES_BIN and a non-root user"]
async fn a_database_left_by_a_killed_engine_is_stopped_before_the_next_starts() {
    second_engine_takes_over(false).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_POSTGRES_BIN and a non-root user"]
async fn with_the_run_directory_cleaned_the_database_is_still_found() {
    second_engine_takes_over(true).await;
}
