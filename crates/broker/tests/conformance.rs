//! The SDK's conformance checks against a test engine in a temporary
//! Capsule directory: the real broker, a real registry and token key on
//! disk, and a supervisor with no services. Never a user's Capsule.
//!
//! Every check runs and the report is printed. The checks for interfaces
//! the engine serves must pass; the rest are listed in `NOT_YET` and must
//! fail, so this list shrinks as each interface lands.

use std::sync::Arc;

use broker::{Apps, Blobs, Services, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile};
use registry::GrantStore;

/// Checks of interfaces the engine does not serve yet.
const NOT_YET: &[&str] = &[
    "events.round_trip",
    "events.scoped",
    "events.shell_reaches_app",
    "subscriptions.owner_only",
    "sql.schemas",
];

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_conforms() {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let stack = Arc::new(supervisor::Stack::new(None));
    let hardware = Arc::new(FixedHardware::with_total(16 << 30));
    let system = StackSystem::new(stack, None, hardware, Profile::Standard);
    let services = Services {
        apps: Apps::new(
            GrantStore::open(&paths.registry_db()).unwrap(),
            TokenKey::load_or_create(&paths.token_key()).unwrap(),
        ),
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
    };
    let socket = paths.engine_socket();
    let bound = broker::bind(&socket).unwrap();
    tokio::spawn(broker::serve(bound, system.clone(), Arc::new(services)));

    let report = isonomic_conformance::run(isonomic_client::unix_connector(&socket)).await;
    println!("{report}");

    let mut wrong = Vec::new();
    for (name, outcome) in &report.results {
        match (NOT_YET.contains(name), outcome) {
            (false, Err(why)) => wrong.push(format!("{name} failed: {why}")),
            (true, Ok(())) => wrong.push(format!("{name} passes: take it off NOT_YET")),
            _ => {}
        }
    }
    assert!(wrong.is_empty(), "{}\n\n{report}", wrong.join("\n"));
}
