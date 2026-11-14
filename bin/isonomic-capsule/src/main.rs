//! The Isonomic engine. Starts tier 0 (registry, broker) and tier 1
//! (PostgreSQL), logs where each bundled binary resolved and how long the
//! cold start took, then serves the `system` interface until asked to stop,
//! and stops everything in reverse order.

use std::process::ExitCode;
use std::sync::Arc;

use broker::StackSystem;
use capsule_core::{EngineConfig, HardwareSource, Profile, SystemHardware, Tier};
use packaging::Bundle;
use registry::ServiceRegistry;
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
    let profile = config
        .profile_override
        .unwrap_or_else(|| Profile::detect(hardware.as_ref()));
    log(format!("hardware profile: {profile}"));

    // Tier 0: the registry and the broker socket. Binding first refuses a
    // second engine early and lets the window watch the services start.
    let registry = Arc::new(ServiceRegistry::open(&paths.registry_db())?);
    let listener = broker::bind(&paths.engine_socket())?;
    log(format!("listening on {}", paths.engine_socket().display()));

    let triple = packaging::host_triple().ok_or("this platform has no bundled services")?;
    let bundle = Bundle::locate(config.bundle_dir.clone(), &std::env::current_exe()?, triple);
    let layout = PostgresLayout {
        postgres: bundle.binary(postgres::NAME, "bin/postgres"),
        initdb: bundle.binary(postgres::NAME, "bin/initdb"),
        data_dir: paths.service_dir(postgres::NAME),
        socket_dir: paths.service_socket_dir(postgres::NAME),
    };
    for (name, path) in [("postgres", &layout.postgres), ("initdb", &layout.initdb)] {
        log(format!(
            "binary {name}: {} ({})",
            path.display(),
            Bundle::status(path).as_str()
        ));
    }

    let prepared = postgres::prepare(&layout, ChildEnv::minimal()).await?;
    if prepared.initialized_now {
        log("postgres: first-time setup done");
    }
    if prepared.unclean_shutdown {
        log("postgres: last shutdown was unclean, allowing recovery time");
    }
    let mut service = prepared.config;
    service.logs_dir = Some(paths.logs_dir());
    let mut stack = Stack::new(Some(Arc::clone(&registry)));
    stack.add(postgres::NAME, Tier::T1, service);
    let stack = Arc::new(stack);

    let system = StackSystem::new(Arc::clone(&stack), Some(registry), hardware, profile);
    let server = tokio::spawn(broker::serve(listener, system.clone()));

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
