//! A Capsule's database made by the earlier bundled build opens unchanged
//! under the one built from source. Not part of the hermetic suite: it
//! needs both builds' binaries and must not run as root.
//!
//!     ISONOMIC_TEST_POSTGRES_OLD_BIN=<old bin dir> ISONOMIC_TEST_POSTGRES_NEW_BIN=<new bin dir> \
//!         cargo test -p appdb --test database_build -- --ignored
//!
//! Each server is started the way the engine starts it (first-time setup,
//! then the supervisor). An app's schema and rows written under the old
//! build are read, and written to, under the new one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use appdb::{Database, PgDatabase, Runner};
use capsule_core::{CapsulePaths, Profile, Tier};
use isonomic_proto::manifest::Schema;
use isonomic_proto::{Capabilities, Manifest, SqlValue};
use supervisor::{postgres, ChildEnv, Stack};

fn notes() -> Manifest {
    Manifest {
        id: "com.isonomic.notes".into(),
        name: "Notes".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities::default(),
        schemas: vec![Schema {
            name: "notes".into(),
        }],
    }
}

fn bin(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")))
}

/// Start the database in `paths` with the server in `bin`.
async fn start(paths: &CapsulePaths, bin: &Path) -> (Arc<Stack>, PgDatabase) {
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
    let db = PgDatabase::new(
        layout.socket_dir,
        postgres::SOCKET_PORT,
        postgres::SUPERUSER,
        postgres::DATABASE,
    );
    (stack, db)
}

async fn version(db: &PgDatabase, app: &Manifest) -> String {
    let rows = db
        .query(Runner::App(app), "notes", "select version()", &[])
        .await
        .unwrap();
    match &rows.rows[0][0] {
        SqlValue::Text(v) => v.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_POSTGRES_OLD_BIN, ISONOMIC_TEST_POSTGRES_NEW_BIN and a non-root user"]
async fn a_database_made_by_the_earlier_build_opens_under_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    paths.create_dirs().unwrap();
    let app = notes();
    let runner = Runner::App(&app);

    let (old, db) = start(&paths, &bin("ISONOMIC_TEST_POSTGRES_OLD_BIN")).await;
    let made_by = version(&db, &app).await;
    let create =
        "create table items (id int8 primary key, body text, at timestamptz default now())";
    db.execute(runner, "notes", create, &[]).await.unwrap();
    let insert = "insert into items (id, body) values ($1, $2)";
    let row = [
        SqlValue::Int(1),
        SqlValue::Text("written by the earlier build".into()),
    ];
    db.execute(runner, "notes", insert, &row).await.unwrap();
    old.shutdown().await;
    drop(db);

    let (new, db) = start(&paths, &bin("ISONOMIC_TEST_POSTGRES_NEW_BIN")).await;
    let opened_by = version(&db, &app).await;
    println!("made by:   {made_by}\nopened by: {opened_by}");
    let read = db
        .query(runner, "notes", "select body from items where id = 1", &[])
        .await
        .unwrap();
    assert_eq!(
        read.rows,
        [[SqlValue::Text("written by the earlier build".into())]]
    );
    let row = [
        SqlValue::Int(2),
        SqlValue::Text("and by the new one".into()),
    ];
    assert_eq!(db.execute(runner, "notes", insert, &row).await.unwrap(), 1);
    let count = db
        .query(runner, "notes", "select count(*) from items", &[])
        .await
        .unwrap();
    assert_eq!(count.rows, [[SqlValue::Int(2)]]);
    new.shutdown().await;
}
