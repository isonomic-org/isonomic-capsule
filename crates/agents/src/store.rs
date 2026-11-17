// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The harness host's `SessionStore` on SQLite: one row per event, appended
//! in one transaction, and one registry row per session.
//!
//! Rows are never updated or deleted except by `purge`. Each event row keeps
//! the schema version it was written under, so a later schema can migrate
//! old rows instead of failing on them.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use harness_core::future::{ready, BoxFuture};
use harness_core::{AppId, Seq, SessionEvent, SessionId, SCHEMA_VERSION};
use harness_log::{SessionMeta, SessionStore, StoreError};
use rusqlite::{params, Connection, OptionalExtension};

pub struct SqliteSessionStore {
    conn: Mutex<Connection>,
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(error.to_string())
}

/// An id's text, as it is written in the log.
fn text<T: serde::Serialize>(id: &T) -> Result<String, StoreError> {
    match serde_json::to_value(id).map_err(backend)? {
        serde_json::Value::String(text) => Ok(text),
        other => Err(backend(format!("an id is not text: {other}"))),
    }
}

impl SqliteSessionStore {
    /// The store in the database at `path`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::init(Connection::open(path)?)
    }

    /// An in-memory store (tests).
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_events (
                session TEXT NOT NULL,
                seq INTEGER NOT NULL,
                schema INTEGER NOT NULL,
                payload TEXT NOT NULL,
                PRIMARY KEY (session, seq)
            ) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                app TEXT NOT NULL,
                meta TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS sessions_by_app ON sessions (app);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn append_now(&self, session: &SessionId, events: &[SessionEvent]) -> Result<(), StoreError> {
        let id = text(session)?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(backend)?;
        let last: Option<Seq> = tx
            .query_row(
                "SELECT MAX(seq) FROM session_events WHERE session = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(backend)?;
        let first = last.map_or(1, |seq| seq + 1);
        for (next, event) in (first..).zip(events) {
            if event.seq != next || &event.session != session {
                return Err(StoreError::Conflict {
                    expected: next,
                    found: event.seq,
                });
            }
            let payload = serde_json::to_string(event).map_err(backend)?;
            tx.execute(
                "INSERT INTO session_events (session, seq, schema, payload)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, event.seq, SCHEMA_VERSION, payload],
            )
            .map_err(backend)?;
        }
        tx.commit().map_err(backend)
    }

    fn read_now(
        &self,
        session: &SessionId,
        after: Seq,
        limit: usize,
    ) -> Result<Vec<SessionEvent>, StoreError> {
        let id = text(session)?;
        let conn = self.lock();
        let exists = conn
            .query_row(
                "SELECT 1 FROM session_events WHERE session = ?1 LIMIT 1",
                params![id],
                |_| Ok(()),
            )
            .optional()
            .map_err(backend)?;
        if exists.is_none() {
            return Err(StoreError::NotFound);
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = conn
            .prepare(
                "SELECT payload FROM session_events WHERE session = ?1 AND seq > ?2
                 ORDER BY seq LIMIT ?3",
            )
            .map_err(backend)?;
        let rows = statement
            .query_map(params![id, after, limit], |row| row.get::<_, String>(0))
            .map_err(backend)?;
        rows.map(|payload| {
            let payload = payload.map_err(backend)?;
            serde_json::from_str(&payload).map_err(backend)
        })
        .collect()
    }

    fn put_meta_now(&self, meta: &SessionMeta) -> Result<(), StoreError> {
        let row = serde_json::to_string(meta).map_err(backend)?;
        self.lock()
            .execute(
                "INSERT INTO sessions (id, app, meta) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET app = excluded.app, meta = excluded.meta",
                params![text(&meta.id)?, text(&meta.app)?, row],
            )
            .map_err(backend)?;
        Ok(())
    }

    fn list_meta_now(&self, owner: Option<&AppId>) -> Result<Vec<SessionMeta>, StoreError> {
        let owner = owner.map(text).transpose()?;
        let conn = self.lock();
        let mut statement = conn
            .prepare("SELECT meta FROM sessions WHERE ?1 IS NULL OR app = ?1 ORDER BY id")
            .map_err(backend)?;
        let rows = statement
            .query_map(params![owner], |row| row.get::<_, String>(0))
            .map_err(backend)?;
        rows.map(|meta| serde_json::from_str(&meta.map_err(backend)?).map_err(backend))
            .collect()
    }

    fn purge_now(&self, sessions: &[SessionId]) -> Result<u64, StoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(backend)?;
        let mut gone = 0;
        for session in sessions {
            let id = text(session)?;
            gone += tx
                .execute("DELETE FROM session_events WHERE session = ?1", params![id])
                .map_err(backend)? as u64;
            tx.execute("DELETE FROM sessions WHERE id = ?1", params![id])
                .map_err(backend)?;
        }
        tx.commit().map_err(backend)?;
        Ok(gone)
    }
}

// SQLite calls are short and local; they run on the caller's task, as the
// registry's do.
impl SessionStore for SqliteSessionStore {
    fn append<'a>(
        &'a self,
        session: &'a SessionId,
        events: &'a [SessionEvent],
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        ready(self.append_now(session, events))
    }

    fn read<'a>(
        &'a self,
        session: &'a SessionId,
        after: Seq,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<SessionEvent>, StoreError>> {
        ready(self.read_now(session, after, limit))
    }

    fn put_meta<'a>(&'a self, meta: &'a SessionMeta) -> BoxFuture<'a, Result<(), StoreError>> {
        ready(self.put_meta_now(meta))
    }

    fn list_meta<'a>(
        &'a self,
        owner: Option<&'a AppId>,
    ) -> BoxFuture<'a, Result<Vec<SessionMeta>, StoreError>> {
        ready(self.list_meta_now(owner))
    }

    fn purge<'a>(&'a self, sessions: &'a [SessionId]) -> BoxFuture<'a, Result<u64, StoreError>> {
        ready(self.purge_now(sessions))
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
