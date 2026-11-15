//! The Isonomic engine. Starts tier 0 (registry, broker) and tier 1
//! (PostgreSQL and the cache and queue service), logs where each bundled binary resolved and how long the
//! cold start took, then serves the `system` interface until asked to stop,
//! and stops everything in reverse order.

use std::process::ExitCode;
use std::sync::Arc;

use broker::StackSystem;
use capsule_core::config::ENV_PROFILE;
use capsule_core::{EngineConfig, HardwareSource, Profile, ProfileRamGate, SystemHardware, Tier};
use packaging::Bundle;
use registry::ServiceRegistry;
use supervisor::cache::{self, CacheLayout};
use supervisor::postgres::{self, PostgresLayout};
use supervisor::{ChildEnv, Stack};
use tokio::signal::unix::{signal, SignalKind};

type AnyError = Box<dyn std::error::Error + Send + Sync>;

fn log(line: impl AsRef<str>) {
    eprintln!("[isonomic-capsule] {}", line.as_ref());
}

async fn run() -> Result<(), AnyError> {
    let config = EngineConfig::from_environment()?;
    let paths = &config.paths;
    paths.create_dirs()?;
    log(format!("capsule directory: {}", paths.root().display()));

    let hardware: Arc<dyn HardwareSource> = Arc::new(SystemHardware);
    if let Some(raw) = &config.profile_override_invalid {
        log(format!(
            "{ENV_PROFILE}={raw:?} is not a profile (low, standard, high); probing instead"
        ));
    }
    let (profile, source) = match config.profile_override {
        Some(profile) => (profile, format!("override {ENV_PROFILE}")),
        None => {
            let total = hardware.probe().total_memory_bytes;
            let gib = total as f64 / (1u64 << 30) as f64;
            (
                Profile::from_total_memory(total),
                format!("detected, {gib:.1} GiB"),
            )
        }
    };
    log(format!("hardware profile: {profile} ({source})"));

    // Tier 0: the registry and the broker socket. Binding first refuses a
    // second engine early and lets the window watch the services start.
    let registry = Arc::new(ServiceRegistry::open(&paths.registry_db())?);
    let listener = match broker::bind(&paths.engine_socket()) {
        Ok(listener) => listener,
        // Not a failure: the engine the user wanted is up. Exiting cleanly
        // keeps the login agent from restarting a duplicate in a loop.
        Err(error @ broker::BindError::AlreadyRunning(_)) => {
            log(format!("{error}; exiting"));
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    log(format!("listening on {}", paths.engine_socket().display()));

    let triple = packaging::host_triple().ok_or("this platform has no bundled services")?;
    let bundle = Bundle::locate(config.bundle_dir.clone(), &std::env::current_exe()?, triple);
    let layout = PostgresLayout {
        postgres: bundle.binary(postgres::NAME, "bin/postgres"),
        initdb: bundle.binary(postgres::NAME, "bin/initdb"),
        data_dir: paths.service_dir(postgres::NAME),
        socket_dir: paths.service_socket_dir(postgres::NAME),
    };
    let cache_layout = CacheLayout {
        program: bundle.binary(cache::SIDECAR, cache::PROGRAM),
        data_dir: paths.service_dir(cache::NAME),
        socket_dir: paths.service_socket_dir(cache::NAME),
    };
    let programs = [
        ("postgres", &layout.postgres),
        ("initdb", &layout.initdb),
        (cache::NAME, &cache_layout.program),
    ];
    for (name, path) in programs {
        log(format!(
            "binary {name}: {} ({})",
            path.display(),
            Bundle::status(path).as_str()
        ));
    }

    let prepared = postgres::prepare(&layout, profile, ChildEnv::minimal()).await?;
    if prepared.initialized_now {
        log("postgres: first-time setup done");
    }
    if prepared.unclean_shutdown {
        log("postgres: last shutdown was unclean, allowing recovery time");
    }
    let mut service = prepared.config;
    service.logs_dir = Some(paths.logs_dir());
    service.pid_file = Some(paths.run_dir().join("postgres.pid"));
    cache::prepare(&cache_layout).await?;
    let mut cache_service = cache::service(&cache_layout, profile, ChildEnv::minimal());
    cache_service.logs_dir = Some(paths.logs_dir());
    cache_service.pid_file = Some(paths.run_dir().join("cache.pid"));

    // The RAM gate sees the chosen profile, override included, and reads free
    // memory at each gated start.
    let gate = ProfileRamGate::new(profile, Arc::clone(&hardware));
    let mut stack = Stack::new(Some(Arc::clone(&registry))).with_gate(Arc::new(gate));
    stack.add(postgres::NAME, Tier::T1, service);
    stack.add(cache::NAME, Tier::T1, cache_service);
    let stack = Arc::new(stack);

    let system = StackSystem::new(Arc::clone(&stack), Some(registry), hardware, profile);
    let apps = broker::Apps::new(
        registry::GrantStore::open(&paths.registry_db())?,
        broker::TokenKey::load_or_create(&paths.token_key())?,
    );
    let server = tokio::spawn(broker::serve(listener, system.clone(), Arc::new(apps)));

    // A failed start does not end the engine: it keeps serving status so the
    // window can show what went wrong.
    match stack.start_tier_0_1().await {
        Ok(elapsed) => {
            system.set_cold_start(elapsed);
            log(format!("cold start: {} ms", elapsed.as_millis()));
        }
        Err(error) => log(format!("core services did not start: {error}")),
    }

    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = system.shutdown_requested() => log("shutdown requested over the socket"),
        _ = terminate.recv() => log("received SIGTERM"),
        _ = interrupt.recv() => log("received SIGINT"),
    }

    stack.shutdown().await;
    server.abort();
    let _ = std::fs::remove_file(paths.engine_socket());
    log("stopped");
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log(format!("fatal: {error}"));
            ExitCode::FAILURE
        }
    }
}
