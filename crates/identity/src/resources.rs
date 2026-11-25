// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the check function protects (R7 steps 3 and 4): one record per
//! thing the person owns, in `registry.db`.
//!
//! The person owns every resource; an app reaches its data, its own
//! included, only through a grant from the person. Each app has a `space`
//! (its collection, and later its encryption scope) holding its schemas,
//! its events, its blobs and its sessions. Rows and event topics are not
//! resources: a statement is checked against its schema, an event against
//! its app's events.
//!
//! A resource is found again by what it is in the engine: its app, kind and
//! name (a schema's name, a blob's handle, a session's id). Schema names are
//! one namespace in the database, so a schema is found by name alone: an app
//! may list another's schema before that app is granted, and the record is
//! claimed by its owner's space when it is.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use id_core::PrincipalId;
use id_resource::{ResourceId, ResourceLookup, ResourceMeta};
use rusqlite::{params, Connection, OptionalExtension};

/// The kinds of resource version 1 has.
pub mod kind {
    pub const SPACE: &str = "space";
    pub const SCHEMA: &str = "schema";
    pub const EVENTS: &str = "events";
    pub const BLOB: &str = "blob";
    pub const SESSION: &str = "session";
}

pub struct Resources {
    conn: Mutex<Connection>,
    owner: PrincipalId,
    /// What has been looked up, so a warm check reads no database. Any
    /// write clears it: resources change rarely (an approval, a schema).
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    found: HashMap<(String, String, String), ResourceId>,
    metas: HashMap<ResourceId, ResourceMeta>,
}

fn id_from(bytes: Vec<u8>) -> Option<ResourceId> {
    let raw: [u8; 16] = bytes.try_into().ok()?;
    Some(ResourceId::from_parts(
        u64::from_be_bytes([0, 0, raw[0], raw[1], raw[2], raw[3], raw[4], raw[5]]),
        raw[6..].try_into().ok()?,
    ))
}

impl Resources {
    /// The resources in the Capsule's `registry.db`, owned by `owner`.
    pub fn open(db: &Path, owner: PrincipalId) -> rusqlite::Result<Self> {
        let conn = Connection::open(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS id_resources (
                id BLOB PRIMARY KEY,
                kind TEXT NOT NULL,
                app TEXT NOT NULL,
                name TEXT NOT NULL,
                parent BLOB,
                collection BLOB NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS id_resources_by_name
                ON id_resources (kind, app, name);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            owner,
            cache: Mutex::new(Cache::default()),
        })
    }

    /// The connection, for a write: what was cached is forgotten.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        *self.cache.lock().unwrap_or_else(|p| p.into_inner()) = Cache::default();
        self.read()
    }

    fn read(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn cached(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whom every resource here belongs to.
    pub fn owner(&self) -> &PrincipalId {
        &self.owner
    }

    /// The resource `kind`/`name` of `app`, if recorded.
    pub fn find(&self, app: &str, kind: &str, name: &str) -> Option<ResourceId> {
        let app = if kind == kind::SCHEMA { "" } else { app };
        let key = (kind.to_string(), app.to_string(), name.to_string());
        if let Some(id) = self.cached().found.get(&key) {
            return Some(*id);
        }
        let id = self
            .read()
            .query_row(
                "SELECT id FROM id_resources WHERE kind = ?1 AND app = ?2 AND name = ?3",
                params![kind, app, name],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(id_from)?;
        self.cached().found.insert(key, id);
        Some(id)
    }

    /// An app's space, recorded if it is not yet.
    pub fn space(&self, app: &str, now_ms: u64) -> rusqlite::Result<ResourceId> {
        if let Some(id) = self.find(app, kind::SPACE, app) {
            return Ok(id);
        }
        let id = ResourceId::new(now_ms);
        self.lock().execute(
            "INSERT INTO id_resources (id, kind, app, name, parent, collection)
             VALUES (?1, ?2, ?3, ?3, NULL, ?1)",
            params![id.as_bytes().to_vec(), kind::SPACE, app],
        )?;
        Ok(id)
    }

    /// `kind`/`name` in `app`'s space, recorded if it is not yet. A schema
    /// recorded earlier without an owner is claimed by this space.
    pub fn within(
        &self,
        app: &str,
        kind: &str,
        name: &str,
        now_ms: u64,
    ) -> rusqlite::Result<ResourceId> {
        let space = self.space(app, now_ms)?;
        let space = space.as_bytes().to_vec();
        // Schemas are found by name alone; their records keep `app` empty.
        let key_app = if kind == kind::SCHEMA { "" } else { app };
        if let Some(id) = self.find(key_app, kind, name) {
            self.lock().execute(
                "UPDATE id_resources SET parent = ?1, collection = ?1 WHERE id = ?2",
                params![space, id.as_bytes().to_vec()],
            )?;
            return Ok(id);
        }
        let id = ResourceId::new(now_ms);
        self.lock().execute(
            "INSERT INTO id_resources (id, kind, app, name, parent, collection)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id.as_bytes().to_vec(), kind, key_app, name, space],
        )?;
        Ok(id)
    }

    /// A schema named by an app that may not be granted yet: recorded on
    /// its own until its owner's space claims it.
    pub fn schema(&self, name: &str, now_ms: u64) -> rusqlite::Result<ResourceId> {
        if let Some(id) = self.find("", kind::SCHEMA, name) {
            return Ok(id);
        }
        let id = ResourceId::new(now_ms);
        self.lock().execute(
            "INSERT INTO id_resources (id, kind, app, name, parent, collection)
             VALUES (?1, ?2, '', ?3, NULL, ?1)",
            params![id.as_bytes().to_vec(), kind::SCHEMA, name],
        )?;
        Ok(id)
    }

    /// Forget a resource (a deleted blob, a purged session).
    pub fn remove(&self, id: &ResourceId) -> rusqlite::Result<()> {
        self.lock().execute(
            "DELETE FROM id_resources WHERE id = ?1",
            params![id.as_bytes().to_vec()],
        )?;
        Ok(())
    }
}

impl ResourceLookup for Resources {
    fn resource(&self, id: &ResourceId) -> Option<ResourceMeta> {
        if let Some(meta) = self.cached().metas.get(id) {
            return Some(meta.clone());
        }
        let row = self
            .read()
            .query_row(
                "SELECT kind, parent, collection FROM id_resources WHERE id = ?1",
                params![id.as_bytes().to_vec()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()
            .ok()??;
        let (kind, parent, collection) = row;
        let meta = ResourceMeta {
            id: *id,
            kind,
            owner: self.owner.clone(),
            parent: parent.and_then(id_from),
            collection: id_from(collection)?,
            derived_from: Vec::new(),
        };
        self.cached().metas.insert(*id, meta.clone());
        Some(meta)
    }
}
