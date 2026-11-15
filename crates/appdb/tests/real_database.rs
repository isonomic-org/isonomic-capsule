//! The apps' database against a real server. Not part of the hermetic
//! suite: it needs the server binaries and must not run as root.
//!
//!     ISONOMIC_TEST_POSTGRES_BIN=<bin dir> cargo test -p appdb --test real_database -- --ignored
//!
//! It starts the server the way the engine does (first-time setup, then
//! the supervisor), then checks that each app's role is the real guard.

use std::path::PathBuf;
use std::sync::Arc;

use appdb::{Database, PgDatabase, Runner};
use capsule_core::{CapsulePaths, Profile, Tier};
use isonomic_proto::manifest::{Schema, SqlAccess};
use isonomic_proto::sql::Statement;
use isonomic_proto::{Capabilities, ErrorCode, Manifest, SqlValue};
use supervisor::{postgres, ChildEnv, Stack};

fn app(name: &str, schemas: &[&str], reads: &[&str]) -> Manifest {
    Manifest {
        id: format!("com.isonomic.{name}"),
        name: name.into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            sql: Some(SqlAccess {
                schemas: reads.iter().map(|s| s.to_string()).collect(),
            }),
            ..Default::default()
        },
        schemas: schemas
            .iter()
            .map(|s| Schema {
                name: s.to_string(),
            })
            .collect(),
    }
}

fn refused(result: Result<impl std::fmt::Debug, isonomic_proto::WireError>, says: &str) {
    match result {
        Err(error) => {
            assert_eq!(error.code, ErrorCode::SqlError, "{error:?}");
            assert!(
                error.message.contains(says),
                "{:?} lacks {says:?}",
                error.message
            );
        }
        Ok(value) => panic!("expected a refusal containing {says:?}, got {value:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ISONOMIC_TEST_POSTGRES_BIN and a non-root user"]
async fn each_apps_role_is_the_real_guard() {
    let bin = PathBuf::from(std::env::var_os("ISONOMIC_TEST_POSTGRES_BIN").expect("bin dir"));
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    paths.create_dirs().unwrap();
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
        layout.socket_dir.clone(),
        postgres::SOCKET_PORT,
        postgres::SUPERUSER,
        postgres::DATABASE,
    );
    let notes = app("notes", &["notes"], &[]);
    let reader = app("reader", &["reader"], &["notes"]);
    let stranger = app("stranger", &["stranger"], &[]);
    let (n, r, s) = (
        Runner::App(&notes),
        Runner::App(&reader),
        Runner::App(&stranger),
    );

    // The reader listed `notes` before it existed: it waits, then catches up.
    db.query(r, "reader", "select 1", &[]).await.unwrap();

    let create = "create table items (id int8 primary key, body text, at timestamptz default now(), price numeric)";
    db.execute(n, "notes", create, &[]).await.unwrap();
    let insert = "insert into items (id, body) values ($1, $2)";
    let params = [SqlValue::Int(1), SqlValue::Text("first".into())];
    assert_eq!(db.execute(n, "notes", insert, &params).await.unwrap(), 1);

    let rows = db
        .query(
            n,
            "notes",
            "select id, body, at from items where id = $1",
            &[SqlValue::Int(1)],
        )
        .await
        .unwrap();
    let kinds: Vec<_> = rows.columns.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(kinds, ["int8", "text", "timestamptz"]);
    assert_eq!(
        rows.rows[0][..2],
        [SqlValue::Int(1), SqlValue::Text("first".into())]
    );
    assert!(matches!(&rows.rows[0][2], SqlValue::Text(t) if t.ends_with('Z')));

    // A listed schema: reads work, writes are the database's to refuse.
    let read = db
        .query(r, "notes", "select body from items", &[])
        .await
        .unwrap();
    assert_eq!(read.rows, [[SqlValue::Text("first".into())]]);
    refused(
        db.execute(r, "notes", "delete from items", &[]).await,
        "permission denied",
    );
    // Through `sql.query` too: the role, not the method, is the guard.
    refused(
        db.query(r, "notes", "delete from items returning id", &[])
            .await,
        "permission denied",
    );
    // An unlisted schema is out of reach, and so is the engine's role.
    refused(
        db.query(s, "stranger", "select * from notes.items", &[])
            .await,
        "permission denied",
    );
    refused(
        db.execute(s, "stranger", "set role isonomic", &[]).await,
        "permission denied",
    );
    refused(
        db.execute(s, "stranger", "create table public.t (x int)", &[])
            .await,
        "permission denied",
    );

    // A batch is all or nothing.
    let batch = [
        Statement {
            sql: insert.into(),
            params: vec![SqlValue::Int(2), SqlValue::Text("second".into())],
        },
        Statement {
            sql: "insert into nope values (1)".into(),
            params: vec![],
        },
    ];
    refused(db.batch(n, "notes", &batch).await, "does not exist");
    let count = db
        .query(n, "notes", "select count(*) as n from items", &[])
        .await
        .unwrap();
    assert_eq!(count.rows, [[SqlValue::Int(1)]]);

    // Two statements in one call are refused, as are types with no wire value.
    refused(
        db.execute(n, "notes", "select 1; drop table items", &[])
            .await,
        "multiple commands",
    );
    refused(
        db.query(n, "notes", "select 1.5::numeric as price", &[])
            .await,
        "cast it to text",
    );

    // A schema another app owns is refused to a newcomer.
    let squatter = app("squatter", &["notes"], &[]);
    refused(
        db.query(Runner::App(&squatter), "notes", "select 1", &[])
            .await,
        "belongs to another app",
    );

    // The shell runs as the engine's role in the schema it names.
    let all = db
        .query(Runner::Shell, "notes", "select count(*) from items", &[])
        .await
        .unwrap();
    assert_eq!(all.rows, [[SqlValue::Int(1)]]);

    stack.shutdown().await;
}
