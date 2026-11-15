//! The SDK's conformance checks against a test engine in a temporary
//! Capsule directory: the real broker, a real registry, token key and blob
//! store on disk. Never a user's Capsule.
//!
//! The hermetic run puts the apps' database behind a stand-in that runs no
//! SQL; the schema rules the `sql.schemas` check probes are decided above
//! it. The opt-in run starts a real database server the way the engine
//! does and runs every check against it:
//!
//!     ISONOMIC_TEST_POSTGRES_BIN=<bin dir> cargo test -p broker --test conformance -- --ignored
//!
//! (The server refuses to run as root.) Every check must pass.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use appdb::{Database, PgDatabase, StandIn};
use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, Tier};
use registry::GrantStore;
use supervisor::{postgres, ChildEnv, Stack};

async fn conform(paths: &CapsulePaths, stack: Arc<Stack>, db: Arc<dyn Database>) {
    let hardware = Arc::new(FixedHardware::with_total(16 << 30));
    let system = StackSystem::new(stack, None, hardware, Profile::Standard);
    let services = Services {
        apps: Arc::new(Apps::new(
            GrantStore::open(&paths.registry_db()).unwrap(),
            TokenKey::load_or_create(&paths.token_key()).unwrap(),
        )),
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(db),
    };
    let socket = paths.engine_socket();
    let bound = broker::bind(&socket).unwrap();
    let server = tokio::spawn(broker::serve(bound, system.clone(), Arc::new(services)));

    let report = isonomic_conformance::run(isonomic_client::unix_connector(&socket)).await;
    println!("{report}");
    server.abort();
    report.assert_passed();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_conforms() {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let stack = Arc::new(Stack::new(None));
    conform(&paths, stack, Arc::new(StandIn::default())).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_POSTGRES_BIN and a non-root user"]
async fn the_engine_conforms_on_a_real_database() {
    let bin = PathBuf::from(std::env::var_os("ISONOMIC_TEST_POSTGRES_BIN").expect("bin dir"));
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    paths.create_dirs().unwrap();
    let (stack, socket_dir) = start_database(&bin, &paths).await;
    let db = PgDatabase::new(
        socket_dir,
        postgres::SOCKET_PORT,
        postgres::SUPERUSER,
        postgres::DATABASE,
    );
    conform(&paths, Arc::clone(&stack), Arc::new(db)).await;
    stack.shutdown().await;
}

async fn start_database(bin: &Path, paths: &CapsulePaths) -> (Arc<Stack>, PathBuf) {
    let layout = postgres::PostgresLayout {
        postgres: bin.join("postgres"),
        initdb: bin.join("initdb"),
        data_dir: paths.service_dir(postgres::NAME),
        socket_dir: paths.service_socket_dir(postgres::NAME),
    };
    let prepared = postgres::prepare(&layout, Profile::Low, ChildEnv::minimal())
        .await
        .unwrap();
    let mut stack = Stack::new(None);
    stack.add(postgres::NAME, Tier::T1, prepared.config);
    let stack = Arc::new(stack);
    stack.start_tier_0_1().await.unwrap();
    (stack, layout.socket_dir)
}
