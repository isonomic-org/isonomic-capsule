//! Manual smoke test against a real PostgreSQL. Not part of the hermetic
//! suite; it needs the server binaries and must not run as root.
//!
//!     cargo run -p supervisor --example pg_smoke -- <postgres bin dir> <scratch dir>
//!
//! Runs first-time setup, a cold start, a force-kill recovery and an ordered
//! stop, and prints what it saw.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use capsule_core::{CapsulePaths, ServiceState, Tier};
use registry::ServiceRegistry;
use supervisor::{postgres, ChildEnv, Stack};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: pg_smoke <postgres bin dir> <scratch dir>";
    let bin = PathBuf::from(args.next().ok_or(usage)?);
    let paths = CapsulePaths::new(args.next().ok_or(usage)?);
    paths.create_dirs()?;

    let layout = postgres::PostgresLayout {
        postgres: bin.join("postgres"),
        initdb: bin.join("initdb"),
        data_dir: paths.service_dir(postgres::NAME),
        socket_dir: paths.service_socket_dir(postgres::NAME),
    };
    let prepared =
        postgres::prepare(&layout, capsule_core::Profile::Low, ChildEnv::minimal()).await?;
    println!(
        "setup: initialized_now={} unclean_shutdown={}",
        prepared.initialized_now, prepared.unclean_shutdown
    );

    let registry = Arc::new(ServiceRegistry::open(&paths.registry_db())?);
    let mut stack = Stack::new(Some(Arc::clone(&registry)));
    let mut config = prepared.config;
    config.logs_dir = Some(paths.logs_dir());
    let pg = stack.add(postgres::NAME, Tier::T1, config);

    let cold = stack.start_tier_0_1().await?;
    let endpoint = pg.endpoint().await.ok_or("no endpoint")?;
    println!("cold start: {} ms on {endpoint}", cold.as_millis());

    let pid = pg.pid().await.ok_or("no pid")?;
    let mut states = pg.subscribe();
    let killed = Instant::now();
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()?;
    states
        .wait_for(|s| *s != ServiceState::Healthy)
        .await
        .map(|_| ())?;
    println!("after kill -9 of pid {pid}: {}", pg.state().as_str());
    tokio::time::timeout(
        Duration::from_secs(120),
        states.wait_for(|s| *s == ServiceState::Healthy),
    )
    .await?
    .map(|_| ())?;
    println!(
        "recovered in {} ms, restarts={}, new pid {:?}",
        killed.elapsed().as_millis(),
        pg.restart_total(),
        pg.pid().await
    );
    println!("registry row: {:?}", registry.get(postgres::NAME)?);

    stack.shutdown().await;
    println!(
        "stopped: {:?}, lock file left behind: {}",
        stack.states(),
        postgres::had_unclean_shutdown(&layout.data_dir)
    );
    Ok(())
}
