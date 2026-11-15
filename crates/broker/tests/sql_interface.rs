//! The `sql` interface over a real socket, above a stand-in database:
//! schema choice, live queries and who may end them. The database's own
//! guard is tested against a real server in `appdb`'s `real_database`.

use std::sync::Arc;
use std::time::Duration;

use appdb::StandIn;
use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile};
use isonomic_client::{unix_connector, Client, Options};
use isonomic_proto::manifest::{Schema, SqlAccess};
use isonomic_proto::sql::Column;
use isonomic_proto::{Call, Capabilities, ErrorCode, Manifest, Reply, Rows, SqlValue};
use registry::GrantStore;

fn manifest(name: &str) -> Manifest {
    Manifest {
        id: format!("com.isonomic.{name}"),
        name: name.into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            sql: Some(SqlAccess::default()),
            ..Default::default()
        },
        schemas: vec![Schema { name: name.into() }],
    }
}

fn count(n: i64) -> Rows {
    Rows {
        columns: vec![Column {
            name: "n".into(),
            kind: "int8".into(),
        }],
        rows: vec![vec![SqlValue::Int(n)]],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_query_follows_writes_and_only_its_opener_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let db = Arc::new(StandIn::default());
    let services = Services {
        apps: Arc::new(Apps::new(
            GrantStore::in_memory().unwrap(),
            TokenKey::fixed([3; 32]),
        )),
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(db.clone()),
        agents: Arc::new(broker::NoAgents),
        models: Arc::new(broker::NoModels),
    };
    let stack = Arc::new(supervisor::Stack::new(None));
    let hardware = Arc::new(FixedHardware::with_total(16 << 30));
    let system = StackSystem::new(stack, None, hardware, Profile::Standard);
    let bound = broker::bind(&paths.engine_socket()).unwrap();
    tokio::spawn(broker::serve(bound, system, Arc::new(services)));

    let shell = Client::start(
        &tokio::runtime::Handle::current(),
        unix_connector(paths.engine_socket()),
        Options::new("test"),
    );
    let notes = shell.scoped(shell.grant(manifest("notes")).await.unwrap());
    let other = shell.scoped(shell.grant(manifest("other")).await.unwrap());

    let query = "select count(*) as n from items";
    db.respond(query, count(1));
    let mut live = notes.sql().live(query, vec![]).await.unwrap();
    assert_eq!(live.rows(), &count(1));

    // A write that changes the result pushes it; one that does not, nothing.
    db.respond(query, count(2));
    notes
        .sql()
        .execute("insert into items default values", vec![])
        .await
        .unwrap();
    let changed = tokio::time::timeout(Duration::from_secs(5), live.changed()).await;
    assert_eq!(changed.unwrap(), Some(&count(2)));
    notes
        .sql()
        .execute("update items set x = 1", vec![])
        .await
        .unwrap();
    let quiet = tokio::time::timeout(Duration::from_millis(300), live.changed()).await;
    assert!(quiet.is_err(), "an unchanged result was sent");

    // Every statement ran as the app, in its own schema.
    assert!(db
        .ran()
        .iter()
        .all(|(app, schema, _)| app.as_deref() == Some("com.isonomic.notes") && schema == "notes"));

    let subscription = live.subscription();
    let theft = other.call(Call::SqlUnsubscribe { subscription }).await;
    match theft {
        Err(isonomic_client::ClientError::Engine(e)) => {
            assert_eq!(e.code, ErrorCode::UnknownSubscription)
        }
        other => panic!("another app ended the live query: {other:?}"),
    }
    let ended = notes.call(Call::SqlUnsubscribe { subscription }).await;
    assert!(matches!(ended, Ok(Reply::Unsubscribed {})));
}
