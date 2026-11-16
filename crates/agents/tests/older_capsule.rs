//! A Capsule left by an earlier build opens in this one (`docs/capsule-format.md`).
//!
//! `fixtures/capsule-format-1/` is format 1 as the first builds wrote it,
//! kept as text: the registry as SQL, the token key in hex, the token Chat
//! was issued and the session it made. The test rebuilds the Capsule from
//! it and opens it with this engine: the marker is added, Chat's old token
//! still works, its session is listed with its title, its log reads back
//! under this harness, and it takes a new message.
//!
//! The fixture is written once, by the build whose format it records:
//!
//!     ISONOMIC_WRITE_FIXTURE=1 cargo test -p agents --test older_capsule -- --ignored
//!
//! A later format adds a new fixture beside it; this one is never rewritten.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use capsule_core::CapsulePaths;
use isonomic_client::{SessionStream, SessionUpdate};
use isonomic_inference_testkit::FakeModelServer;
use isonomic_proto::agents::SessionId;
use isonomic_proto::Token;
use rusqlite::types::ValueRef;

const TITLE: &str = "Lisbon plan";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/capsule-format-1")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(fixture().join(name))
        .unwrap()
        .trim()
        .to_string()
}

/// Event types until `turn.end`, and its stop.
async fn turn(stream: &mut SessionStream) -> (Vec<String>, String) {
    let mut seen = Vec::new();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
        match next.expect("no update within 10 s").expect("stream ended") {
            SessionUpdate::Event(e) => {
                let value = serde_json::to_value(&*e).unwrap();
                let kind = value["type"].as_str().unwrap_or("").to_string();
                seen.push(kind.clone());
                if kind == "turn.end" {
                    return (seen, value["stop"].as_str().unwrap_or("").to_string());
                }
            }
            SessionUpdate::Delta(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_format_one_capsule_opens_with_its_apps_and_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    // The Capsule as the earlier build left it: no marker.
    let db = rusqlite::Connection::open(paths.registry_db()).unwrap();
    db.execute_batch(&read("registry.sql")).unwrap();
    drop(db);
    let key: Vec<u8> = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&read("token.key.hex")[i..i + 2], 16).unwrap())
        .collect();
    std::fs::create_dir_all(paths.token_key().parent().unwrap()).unwrap();
    std::fs::write(paths.token_key(), key).unwrap();

    let marker = capsule_core::format::open(&paths, "next").unwrap();
    assert_eq!((marker.format, marker.created_by.as_str()), (1, "unknown"));

    let server = FakeModelServer::start().await;
    let shell = support::shell(dir.path(), server.port()).await;
    // The token issued by the earlier build, never granted again.
    let chat = shell.scoped(Token::new(read("chat.token")));
    let session = SessionId::from(read("session.txt").as_str());
    let (listed, _) = chat.agents().list(Default::default()).await.unwrap();
    let summary = listed
        .iter()
        .find(|s| s.id == session)
        .expect("the earlier session is listed");
    assert_eq!(summary.title.as_deref(), Some(TITLE));

    let mut stream = chat.agents().subscribe(&session, 0).await.unwrap();
    let (earlier, stop) = turn(&mut stream).await;
    assert_eq!(stop, "done", "the earlier turn: {earlier:?}");
    assert!(earlier.iter().any(|k| k == "assistant"), "{earlier:?}");

    chat.agents()
        .send_text(&session, "And a second day?")
        .await
        .unwrap();
    let (now, stop) = turn(&mut stream).await;
    assert_eq!(stop, "done", "the new turn: {now:?}");

    // A Capsule from before Isonomic IDs gets one on first open, then the
    // same one.
    let made = shell.identity().await.unwrap();
    assert!(made.created && made.id.starts_with("did:iso:"), "{made:?}");
    let again = shell.identity().await.unwrap();
    assert_eq!((again.id, again.created), (made.id, false));
}

/// Write the fixture from this build. Run once per format, by hand.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes the fixture; set ISONOMIC_WRITE_FIXTURE=1"]
async fn write_the_format_one_fixture() {
    if std::env::var_os("ISONOMIC_WRITE_FIXTURE").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    let server = FakeModelServer::start().await;
    let shell = support::shell(dir.path(), server.port()).await;
    let token = shell.grant(support::chat_manifest()).await.unwrap();
    let chat = shell.scoped(token.clone());
    let session = chat.agents().create("chat").await.unwrap();
    let mut stream = chat.agents().subscribe(&session, 0).await.unwrap();
    chat.agents()
        .send_text(&session, "Say hello")
        .await
        .unwrap();
    assert_eq!(turn(&mut stream).await.1, "done");
    chat.agents().rename(&session, TITLE).await.unwrap();

    let out = fixture();
    std::fs::create_dir_all(&out).unwrap();
    let db = rusqlite::Connection::open(paths.registry_db()).unwrap();
    std::fs::write(out.join("registry.sql"), dump(&db)).unwrap();
    let key = std::fs::read(paths.token_key()).unwrap();
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(out.join("token.key.hex"), hex + "\n").unwrap();
    std::fs::write(out.join("chat.token"), format!("{}\n", token.as_str())).unwrap();
    std::fs::write(out.join("session.txt"), format!("{session}\n")).unwrap();
}

/// The database as SQL: each table's definition and rows, then indexes.
fn dump(db: &rusqlite::Connection) -> String {
    let mut out = String::from("BEGIN;\n");
    let mut objects = db
        .prepare(
            "SELECT type, name, sql FROM sqlite_master \
             WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' \
             ORDER BY type = 'index', name",
        )
        .unwrap();
    let objects: Vec<(String, String, String)> = objects
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for (kind, name, sql) in objects {
        out.push_str(&format!("{sql};\n"));
        if kind != "table" {
            continue;
        }
        let mut rows = db.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
        let columns = rows.column_count();
        let mut rows = rows.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let values: Vec<String> = (0..columns)
                .map(|i| match row.get_ref(i).unwrap() {
                    ValueRef::Null => "NULL".into(),
                    ValueRef::Integer(n) => n.to_string(),
                    ValueRef::Real(f) => f.to_string(),
                    ValueRef::Text(t) => {
                        format!("'{}'", String::from_utf8_lossy(t).replace('\'', "''"))
                    }
                    ValueRef::Blob(b) => {
                        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                        format!("X'{hex}'")
                    }
                })
                .collect();
            out.push_str(&format!(
                "INSERT INTO \"{name}\" VALUES ({});\n",
                values.join(", ")
            ));
        }
    }
    out.push_str("COMMIT;\n");
    out
}
