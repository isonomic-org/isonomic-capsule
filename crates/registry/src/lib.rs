// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Embedded state store (architecture spec, section 4.4): what the engine
//! observed about each service. Allocated port or socket, lifecycle state,
//! restart count and last-healthy time, in SQLite inside the engine process.
//! Ports are allocated at run time and recorded here, never hardcoded.
//!
//! Writers persist here before they notify any subscriber, so nobody woken by
//! a notification can read a stale row.

pub mod check;
mod grants;
pub mod salvage;

pub use grants::{Grant, GrantStore};

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};

use capsule_core::{Clock, ServiceState, SystemClock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRecord {
    pub name: String,
    pub port: Option<u16>,
    /// Unix socket path, for services that listen on one.
    pub socket: Option<String>,
    pub state: String,
    pub restart_count: u32,
    pub last_healthy_unix: Option<i64>,
}

pub struct ServiceRegistry {
    conn: Mutex<Connection>,
    clock: Arc<dyn Clock>,
}

impl ServiceRegistry {
    /// On-disk registry under the app-data root.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::init(Connection::open(path)?)
    }

    /// In-memory registry (tests).
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Replace the clock that stamps `last_healthy_unix` (tests).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS services (\n                name TEXT PRIMARY KEY,\n                port INTEGER,\n                socket TEXT,\n                state TEXT NOT NULL DEFAULT 'stopped',\n                restart_count INTEGER NOT NULL DEFAULT 0,\n                last_healthy_unix INTEGER\n            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            clock: Arc::new(SystemClock),
        })
    }

    fn ensure_row(conn: &Connection, name: &str) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT OR IGNORE INTO services (name) VALUES (?1)",
            params![name],
        )?;
        Ok(())
    }

    pub fn record_port(&self, name: &str, port: u16) -> rusqlite::Result<()> {
        let conn = self.conn.lock().expect("registry lock");
        Self::ensure_row(&conn, name)?;
        conn.execute(
            "UPDATE services SET port = ?2 WHERE name = ?1",
            params![name, port],
        )?;
        Ok(())
    }

    pub fn record_socket(&self, name: &str, socket: &Path) -> rusqlite::Result<()> {
        let conn = self.conn.lock().expect("registry lock");
        Self::ensure_row(&conn, name)?;
        conn.execute(
            "UPDATE services SET socket = ?2 WHERE name = ?1",
            params![name, socket.display().to_string()],
        )?;
        Ok(())
    }

    pub fn record_state(&self, name: &str, state: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().expect("registry lock");
        Self::ensure_row(&conn, name)?;
        conn.execute(
            "UPDATE services SET state = ?2 WHERE name = ?1",
            params![name, state],
        )?;
        Ok(())
    }

    pub fn record_restart(&self, name: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().expect("registry lock");
        Self::ensure_row(&conn, name)?;
        conn.execute(
            "UPDATE services SET restart_count = restart_count + 1 WHERE name = ?1",
            params![name],
        )?;
        Ok(())
    }

    pub fn record_healthy(&self, name: &str) -> rusqlite::Result<()> {
        let now = self.clock.now_unix();
        let conn = self.conn.lock().expect("registry lock");
        Self::ensure_row(&conn, name)?;
        conn.execute(
            "UPDATE services SET state = 'healthy', last_healthy_unix = ?2 WHERE name = ?1",
            params![name, now],
        )?;
        Ok(())
    }

    pub fn get(&self, name: &str) -> rusqlite::Result<Option<ServiceRecord>> {
        let conn = self.conn.lock().expect("registry lock");
        conn.query_row(
            "SELECT name, port, socket, state, restart_count, last_healthy_unix FROM services WHERE name = ?1",
            params![name],
            |row| {
                Ok(ServiceRecord {
                    name: row.get(0)?,
                    port: row.get::<_, Option<u16>>(1)?,
                    socket: row.get(2)?,
                    state: row.get(3)?,
                    restart_count: row.get(4)?,
                    last_healthy_unix: row.get(5)?,
                })
            },
        )
        .optional()
    }

    /// Every recorded service, in name order.
    pub fn all(&self) -> rusqlite::Result<Vec<ServiceRecord>> {
        let conn = self.conn.lock().expect("registry lock");
        let mut stmt = conn.prepare(
            "SELECT name, port, socket, state, restart_count, last_healthy_unix FROM services ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ServiceRecord {
                name: row.get(0)?,
                port: row.get::<_, Option<u16>>(1)?,
                socket: row.get(2)?,
                state: row.get(3)?,
                restart_count: row.get(4)?,
                last_healthy_unix: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// Aggregate status light over every recorded service, using the same rule
    /// as `Stack::aggregate_state` but read from the persisted rows — so the
    /// window shows the supervisor's real state even though it is not the code
    /// that owns the processes. `None` means nothing is registered yet.
    pub fn aggregate_state(&self) -> rusqlite::Result<Option<ServiceState>> {
        let states: Vec<ServiceState> = self
            .all()?
            .iter()
            .map(|record| ServiceState::parse(&record.state).unwrap_or(ServiceState::Stopped))
            .collect();
        if states.is_empty() {
            return Ok(None);
        }
        let aggregate = if states.contains(&ServiceState::Degraded) {
            ServiceState::Degraded
        } else if states.iter().all(|s| *s == ServiceState::Healthy) {
            ServiceState::Healthy
        } else if states.contains(&ServiceState::Starting) {
            ServiceState::Starting
        } else {
            ServiceState::Stopped
        };
        Ok(Some(aggregate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_port_state_restarts_and_health() {
        let registry = ServiceRegistry::in_memory().unwrap();
        assert!(registry.get("cache").unwrap().is_none());

        registry.record_port("cache", 40123).unwrap();
        registry.record_state("cache", "starting").unwrap();
        registry.record_restart("cache").unwrap();
        registry.record_restart("cache").unwrap();
        registry.record_healthy("cache").unwrap();

        let record = registry.get("cache").unwrap().unwrap();
        assert_eq!(record.port, Some(40123));
        assert_eq!(record.state, "healthy");
        assert_eq!(record.restart_count, 2);
        assert!(record.last_healthy_unix.unwrap() > 0);
    }

    #[test]
    fn survives_reopen_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.sqlite");
        {
            let registry = ServiceRegistry::open(&path).unwrap();
            registry.record_port("cache", 40124).unwrap();
        }
        let registry = ServiceRegistry::open(&path).unwrap();
        assert_eq!(registry.get("cache").unwrap().unwrap().port, Some(40124));
    }

    #[test]
    fn aggregate_state_follows_the_status_light_rule() {
        let registry = ServiceRegistry::in_memory().unwrap();
        assert_eq!(registry.aggregate_state().unwrap(), None);

        registry.record_healthy("gateway").unwrap();
        registry.record_healthy("cache").unwrap();
        assert_eq!(
            registry.aggregate_state().unwrap(),
            Some(ServiceState::Healthy)
        );
        assert_eq!(registry.all().unwrap().len(), 2);

        registry.record_state("cache", "starting").unwrap();
        assert_eq!(
            registry.aggregate_state().unwrap(),
            Some(ServiceState::Starting)
        );

        registry.record_state("gateway", "degraded").unwrap();
        assert_eq!(
            registry.aggregate_state().unwrap(),
            Some(ServiceState::Degraded)
        );

        registry.record_state("gateway", "stopped").unwrap();
        registry.record_state("cache", "stopped").unwrap();
        assert_eq!(
            registry.aggregate_state().unwrap(),
            Some(ServiceState::Stopped)
        );
    }

    #[test]
    fn a_socket_is_recorded_and_healthy_is_stamped_by_the_injected_clock() {
        let clock = Arc::new(capsule_core::ManualClock::at(1_700_000_000));
        let registry = ServiceRegistry::in_memory()
            .unwrap()
            .with_clock(clock.clone());
        registry
            .record_socket("postgres", Path::new("/c/run/postgres/.s.PGSQL.5432"))
            .unwrap();
        registry.record_healthy("postgres").unwrap();
        let record = registry.get("postgres").unwrap().unwrap();
        assert_eq!(record.port, None);
        assert_eq!(
            record.socket.as_deref(),
            Some("/c/run/postgres/.s.PGSQL.5432")
        );
        assert_eq!(record.last_healthy_unix, Some(1_700_000_000));

        clock.advance(45);
        registry.record_healthy("postgres").unwrap();
        let later = registry.get("postgres").unwrap().unwrap();
        assert_eq!(later.last_healthy_unix, Some(1_700_000_045));
    }
}
