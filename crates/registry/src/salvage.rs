//! What can still be read from a registry that failed its check, copied
//! into the new one before the engine starts, so a disk fault costs as
//! little as possible: the chats first, then the list of stored files, then
//! the approvals.
//!
//! The set-aside copy is never opened. The work is done on a throwaway
//! duplicate of it (with its `-wal`, so the latest writes are read too),
//! which is removed at the end.
//!
//! A damaged page in SQLite costs only the rows on it, if each row is
//! looked up by its key rather than read in one scan, so:
//!
//! - **Tables with rowids** (sessions, stored files, approvals, tokens, ID
//!   resources) are read one rowid at a time. A row that fails to read is
//!   counted as lost, and a row that is not there was deleted.
//! - **`session_events`** has no rowids. For each recovered chat, its events
//!   are read one sequence number at a time, upward, until [`GAP`] numbers
//!   in a row are absent.
//! - **Indexes** are made anew after the rows are in, so a damaged index
//!   costs nothing.
//!
//! `services` is not copied: it is the engine's own observations, and is
//! rebuilt at start.

use std::path::Path;

use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, OpenFlags, OptionalExtension};

/// How many absent keys in a row end a probe.
pub const GAP: i64 = 64;
/// How many keys in a row that fail or are absent end a probe whatever
/// else it knows: a table whose root is damaged fails every lookup.
pub const GIVE_UP: i64 = 10_000;

/// Tables copied, most important first. Others are copied after them.
const FIRST: &[&str] = &["sessions", "blobs", "grants", "tokens", "id_resources"];
/// Tables never copied.
const SKIP: &[&str] = &["services"];

/// What a salvage got back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Salvage {
    /// Chats recovered.
    pub chats: u64,
    /// Of those, how many lost some of their history.
    pub chats_partial: u64,
    /// Chats that could not be read.
    pub chats_lost: u64,
    /// Approved apps recovered, and approvals that could not be read.
    pub apps: u64,
    pub apps_lost: u64,
    /// Stored files recovered and lost.
    pub files: u64,
    pub files_lost: u64,
    /// Whether anything at all could be read (the schema survived).
    pub readable: bool,
    /// Tables that failed to read with no way to count what was lost.
    pub uncounted: Vec<String>,
}

/// Copy what can be read from `damaged` into a new registry at `into`,
/// working on a duplicate in `work`.
pub fn salvage(damaged: &Path, into: &Path, work: &Path) -> std::io::Result<Salvage> {
    std::fs::create_dir_all(work)?;
    let copy = work.join("registry.db");
    for suffix in ["", "-wal", "-shm"] {
        let from = format!("{}{suffix}", damaged.display());
        if Path::new(&from).exists() {
            std::fs::copy(&from, format!("{}{suffix}", copy.display()))?;
        }
    }
    let result = salvage_from(&copy, into);
    let _ = std::fs::remove_dir_all(work);
    Ok(result.unwrap_or_default())
}

fn salvage_from(copy: &Path, into: &Path) -> rusqlite::Result<Salvage> {
    let source = Connection::open_with_flags(copy, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .or_else(|_| Connection::open_with_flags(copy, OpenFlags::SQLITE_OPEN_READ_ONLY))?;
    let mut found = Salvage::default();
    let schema = match schema(&source) {
        Ok(schema) if !schema.is_empty() => schema,
        _ => return Ok(found),
    };
    found.readable = true;
    let mut target = Connection::open(into)?;
    let tx = target.transaction()?;
    let mut tables: Vec<&(String, String, String)> =
        schema.iter().filter(|(kind, ..)| kind == "table").collect();
    tables.sort_by_key(|(_, name, _)| FIRST.iter().position(|f| f == name).unwrap_or(FIRST.len()));
    for (_, name, sql) in &tables {
        if SKIP.contains(&name.as_str()) {
            continue;
        }
        tx.execute_batch(sql)?;
        if name == "session_events" {
            continue; // after the chats, by their ids
        }
        let (copied, lost) = copy_by_rowid(&source, &tx, name, &mut found.uncounted)?;
        match name.as_str() {
            "sessions" => (found.chats, found.chats_lost) = (copied, lost),
            "grants" => (found.apps, found.apps_lost) = (copied, lost),
            "blobs" => (found.files, found.files_lost) = (copied, lost),
            _ => {}
        }
    }
    if tables.iter().any(|(_, name, _)| name == "session_events") {
        found.chats_partial = copy_events(&source, &tx)?;
    }
    for (kind, _, sql) in &schema {
        if kind == "index" {
            let _ = tx.execute_batch(sql);
        }
    }
    tx.commit()?;
    Ok(found)
}

/// The damaged file's own definitions: (kind, name, sql), tables and
/// indexes it made itself.
fn schema(source: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut statement = source.prepare(
        "SELECT type, name, sql FROM sqlite_master
         WHERE type IN ('table', 'index') AND sql IS NOT NULL
           AND name NOT LIKE 'sqlite_%' ORDER BY type DESC, rowid",
    )?;
    let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    rows.collect()
}

/// One row's values, `None` when absent, an error when unreadable.
fn row(source: &Connection, sql: &str, key: &[Value]) -> rusqlite::Result<Option<Vec<Value>>> {
    let mut statement = source.prepare_cached(sql)?;
    let width = statement.column_count();
    statement
        .query_row(params_from_iter(key.iter()), |r| {
            (0..width).map(|i| r.get::<_, Value>(i)).collect()
        })
        .optional()
}

fn insert(target: &Connection, table: &str, values: &[Value]) -> rusqlite::Result<()> {
    let marks = vec!["?"; values.len()].join(", ");
    let sql = format!("INSERT OR IGNORE INTO \"{table}\" VALUES ({marks})");
    target
        .prepare_cached(&sql)?
        .execute(params_from_iter(values.iter()))?;
    Ok(())
}

/// Every row of a rowid table that still reads: (copied, unreadable). A
/// failed row is counted only below the highest rowid known, so a table
/// that cannot be read at all is not counted here: it goes in `uncounted`.
fn copy_by_rowid(
    source: &Connection,
    target: &Connection,
    table: &str,
    uncounted: &mut Vec<String>,
) -> rusqlite::Result<(u64, u64)> {
    let has_rowid = source
        .prepare(&format!("SELECT rowid FROM \"{table}\" LIMIT 0"))
        .is_ok();
    if !has_rowid {
        return Ok((0, 0));
    }
    // The highest rowid, if the right edge of the table still reads.
    let highest: Option<i64> = source
        .query_row(&format!("SELECT max(rowid) FROM \"{table}\""), [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .ok()
        .flatten();
    let sql = format!("SELECT * FROM \"{table}\" WHERE rowid = ?1");
    let (mut copied, mut absent, mut since, mut rowid) = (0u64, 0i64, 0i64, 0i64);
    let (mut last_found, mut failed) = (0i64, Vec::new());
    loop {
        let past_end = rowid >= highest.unwrap_or(0);
        if since >= GIVE_UP || (past_end && absent >= GAP) {
            break;
        }
        rowid += 1;
        match row(source, &sql, &[Value::Integer(rowid)]) {
            Ok(Some(values)) => {
                insert(target, table, &values)?;
                copied += 1;
                (absent, since, last_found) = (0, 0, rowid);
            }
            Ok(None) => (absent, since) = (absent + 1, since + 1),
            Err(_) => {
                failed.push(rowid);
                (absent, since) = (0, since + 1);
            }
        }
    }
    let bound = highest.unwrap_or(0).max(last_found);
    let lost = failed.iter().filter(|&&r| r <= bound).count() as u64;
    if failed.len() as u64 > lost {
        uncounted.push(table.to_string());
    }
    Ok((copied, lost))
}

/// The events of every recovered chat, one sequence number at a time; how
/// many chats lost some of theirs.
fn copy_events(source: &Connection, target: &Connection) -> rusqlite::Result<u64> {
    let ids: Vec<String> = {
        let mut statement = target.prepare("SELECT id FROM sessions")?;
        let rows = statement.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let sql = "SELECT * FROM session_events WHERE session = ?1 AND seq = ?2";
    let mut partial = 0;
    for id in ids {
        let (mut seq, mut absent, mut since, mut lost) = (-1i64, 0i64, 0i64, false);
        while absent < GAP && since < GIVE_UP {
            seq += 1;
            match row(source, sql, &[Value::Text(id.clone()), Value::Integer(seq)]) {
                Ok(Some(values)) => {
                    insert(target, "session_events", &values)?;
                    (absent, since) = (0, 0);
                }
                Ok(None) => (absent, since) = (absent + 1, since + 1),
                Err(_) => {
                    lost = true;
                    (absent, since) = (0, since + 1);
                }
            }
        }
        partial += u64::from(lost);
    }
    Ok(partial)
}
