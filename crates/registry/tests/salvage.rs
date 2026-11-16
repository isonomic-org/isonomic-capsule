//! A registry damaged in different places gives back what it still can:
//! the header (nothing), one page of the chats table (all but that page's
//! chats), one page of one chat's history (that chat, marked as missing
//! part of it, and every other chat whole), the index only (everything). The set-aside copy is never written.
//! The registry is built with the engine's own table definitions.

use std::path::Path;

use registry::check::{check_or_set_aside, damage};
use rusqlite::Connection;

const PAGE: usize = 4096;
const CHATS: usize = 300;
const EVENTS: usize = 5;
const FILES: usize = 50;

/// The engine's tables, as the crates that own them make them.
const SCHEMA: &str = "
    CREATE TABLE grants (app TEXT PRIMARY KEY, manifest TEXT NOT NULL);
    CREATE TABLE tokens (digest TEXT PRIMARY KEY, app TEXT NOT NULL REFERENCES grants(app));
    CREATE TABLE session_events (
        session TEXT NOT NULL, seq INTEGER NOT NULL, schema INTEGER NOT NULL,
        payload TEXT NOT NULL, PRIMARY KEY (session, seq)
    ) WITHOUT ROWID;
    CREATE TABLE sessions (id TEXT PRIMARY KEY, app TEXT NOT NULL, meta TEXT NOT NULL);
    CREATE TABLE blobs (handle TEXT PRIMARY KEY, owner TEXT, content_type TEXT NOT NULL,
        size INTEGER NOT NULL, written INTEGER NOT NULL);
    CREATE TABLE services (name TEXT PRIMARY KEY, state TEXT NOT NULL);";

/// A registry at `db`; the pages the chats table fills, first and last.
fn registry(db: &Path) -> (usize, usize) {
    let conn = Connection::open(db).unwrap();
    conn.execute_batch(&format!("PRAGMA page_size = {PAGE}; {SCHEMA}"))
        .unwrap();
    let pages = |c: &Connection| {
        c.query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))
            .unwrap() as usize
    };
    for app in [
        "com.isonomic.chat",
        "com.isonomic.notes",
        "com.isonomic.reader",
    ] {
        conn.execute("INSERT INTO grants VALUES (?1, '{}')", [app])
            .unwrap();
        conn.execute(
            "INSERT INTO tokens VALUES (?1, ?2)",
            [format!("digest-{app}"), app.into()],
        )
        .unwrap();
    }
    let before = pages(&conn);
    let meta = "m".repeat(1000);
    for i in 0..CHATS {
        conn.execute(
            "INSERT INTO sessions VALUES (?1, 'com.isonomic.chat', ?2)",
            [format!("ses_{i:04}"), meta.clone()],
        )
        .unwrap();
    }
    let after = pages(&conn);
    let payload = "p".repeat(300);
    for i in 0..CHATS {
        for seq in 0..EVENTS {
            conn.execute(
                "INSERT INTO session_events VALUES (?1, ?2, 1, ?3)",
                rusqlite::params![format!("ses_{i:04}"), seq as i64, payload],
            )
            .unwrap();
        }
    }
    for i in 0..FILES {
        conn.execute(
            "INSERT INTO blobs VALUES (?1, 'com.isonomic.chat', 'text/plain', 10, 0)",
            [format!("blob_{i}")],
        )
        .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO services VALUES ('postgres', 'healthy');
         CREATE INDEX sessions_by_app ON sessions (app);",
    )
    .unwrap();
    (before + 1, after)
}

/// Overwrite page `n` (1-based) with junk.
fn spoil_page(db: &Path, n: usize) {
    let mut bytes = std::fs::read(db).unwrap();
    let at = (n - 1) * PAGE;
    bytes[at..at + PAGE].fill(0xab);
    std::fs::write(db, bytes).unwrap();
}

fn count(db: &Path, table: &str) -> usize {
    let conn = Connection::open(db).unwrap();
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap() as usize
}

/// Set the damaged registry aside and salvage it; the set-aside copy must
/// be the damaged file byte for byte, and no working copy may be left.
fn set_aside(dir: &Path) -> registry::check::SetAside {
    let db = dir.join("registry.db");
    let damaged = std::fs::read(&db).unwrap();
    assert!(damage(&db).is_some(), "the fixture is damaged");
    let unreadable = dir.join("unreadable");
    let set = check_or_set_aside(&db, &unreadable, "20261116T071508Z")
        .unwrap()
        .expect("set aside");
    assert_eq!(std::fs::read(&set.to).unwrap(), damaged, "never written");
    let left: Vec<_> = std::fs::read_dir(&unreadable)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(left.len(), 1, "only the set-aside copy remains: {left:?}");
    set
}

#[test]
fn a_damaged_header_gives_back_nothing_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("registry.db");
    registry(&db);
    let mut bytes = std::fs::read(&db).unwrap();
    bytes[..100].fill(0xab);
    std::fs::write(&db, bytes).unwrap();
    let set = set_aside(dir.path());
    assert!(!set.salvage.readable);
    let message = set.message();
    assert!(message.contains("nothing in it could be read"), "{message}");
    assert!(message.contains("Apps must be approved again"), "{message}");
}

#[test]
fn one_damaged_page_of_chats_costs_only_that_pages_chats() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("registry.db");
    let (first, last) = registry(&db);
    spoil_page(&db, (first + last) / 2);
    let set = set_aside(dir.path());
    let got = &set.salvage;
    assert!(got.readable);
    assert!(got.chats_lost >= 1 && got.chats_lost <= 10, "{got:?}");
    assert_eq!(got.chats + got.chats_lost, CHATS as u64, "{got:?}");
    assert_eq!(got.chats_partial, 0, "the events were not damaged");
    assert_eq!((got.apps, got.apps_lost), (3, 0));
    assert_eq!((got.files, got.files_lost), (FILES as u64, 0));
    assert_eq!(damage(&db), None, "the new registry is whole");
    assert_eq!(count(&db, "sessions"), got.chats as usize);
    assert_eq!(count(&db, "session_events"), got.chats as usize * EVENTS);
    assert_eq!(count(&db, "tokens"), 3);
    let message = set.message();
    assert!(
        message.contains(&format!(
            "{} chats recovered, {} could not be read",
            got.chats, got.chats_lost
        )),
        "{message}"
    );
    assert!(
        message.contains("Approved apps keep their approvals"),
        "{message}"
    );
}

#[test]
fn a_damaged_index_costs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("registry.db");
    registry(&db);
    let pages = std::fs::metadata(&db).unwrap().len() as usize / PAGE;
    spoil_page(&db, pages); // the index is made last, so its pages are last
    let set = set_aside(dir.path());
    let got = &set.salvage;
    assert_eq!(
        (got.chats, got.chats_lost, got.chats_partial),
        (CHATS as u64, 0, 0),
        "{got:?}"
    );
    assert_eq!(damage(&db), None);
    let conn = Connection::open(&db).unwrap();
    let index: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'sessions_by_app'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(index, 1, "the index is made anew");
    assert_eq!(count(&db, "session_events"), CHATS * EVENTS);
    assert!(
        set.message().contains("300 chats recovered."),
        "{}",
        set.message()
    );
}

/// One chat with a long history, added after the rest, so its later events
/// fill pages of their own; the pages they fill, first and last.
fn long_history(db: &Path, chat: &str, events: usize) -> (usize, usize) {
    let conn = Connection::open(db).unwrap();
    let pages = |c: &Connection| {
        c.query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))
            .unwrap() as usize
    };
    let before = pages(&conn);
    let payload = "q".repeat(300);
    for seq in EVENTS..EVENTS + events {
        conn.execute(
            "INSERT INTO session_events VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![chat, seq as i64, payload],
        )
        .unwrap();
    }
    (before + 1, pages(&conn))
}

#[test]
fn a_damaged_page_in_one_chats_history_costs_only_part_of_that_chat() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("registry.db");
    registry(&db);
    let long = 200;
    let (first, last) = long_history(&db, "ses_0150", long);
    assert!(
        last - first >= 10,
        "the long history spans pages {first}..{last}"
    );
    spoil_page(&db, (first + last) / 2);
    let set = set_aside(dir.path());
    let got = &set.salvage;
    assert_eq!((got.chats, got.chats_lost), (CHATS as u64, 0), "{got:?}");
    assert_eq!(got.chats_partial, 1, "{got:?}");

    let conn = Connection::open(&db).unwrap();
    let events = |chat: &str| -> usize {
        conn.query_row(
            "SELECT count(*) FROM session_events WHERE session = ?1",
            [chat],
            |r| r.get::<_, i64>(0),
        )
        .unwrap() as usize
    };
    let kept = events("ses_0150");
    assert!(kept > EVENTS && kept < EVENTS + long, "{kept} events kept");
    // Its events on both sides of the damaged page come back.
    let newest: i64 = conn
        .query_row(
            "SELECT max(seq) FROM session_events WHERE session = 'ses_0150'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        newest as usize,
        EVENTS + long - 1,
        "the events past the damage"
    );
    for other in ["ses_0000", "ses_0149", "ses_0151", "ses_0299"] {
        assert_eq!(events(other), EVENTS, "{other} is whole");
    }
    let message = set.message();
    assert!(
        message.contains("300 chats (1 missing part of their history) recovered."),
        "{message}"
    );
}
