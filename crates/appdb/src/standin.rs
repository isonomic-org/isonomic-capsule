//! A database that runs no SQL, for hermetic tests of the layers above:
//! reads answer what a test scripted (no rows otherwise), writes change
//! nothing, and every statement is recorded with who ran it and where.

use std::collections::HashMap;
use std::sync::Mutex;

use isonomic_proto::sql::Statement;
use isonomic_proto::{Manifest, Rows, SqlValue, WireError};

use crate::{BoxFuture, Database, Runner};

/// One statement as the stand-in received it: the app (`None` for the
/// shell), the schema and the text.
pub type Ran = (Option<String>, String, String);

pub struct StandIn {
    scripted: Mutex<HashMap<String, Rows>>,
    ran: Mutex<Vec<Ran>>,
    prepared: Mutex<Vec<String>>,
    written: tokio::sync::broadcast::Sender<()>,
}

impl Default for StandIn {
    fn default() -> Self {
        Self {
            scripted: Mutex::default(),
            ran: Mutex::default(),
            prepared: Mutex::default(),
            written: tokio::sync::broadcast::channel(64).0,
        }
    }
}

impl StandIn {
    /// Answer `sql` with `rows` from now on.
    pub fn respond(&self, sql: &str, rows: Rows) {
        self.scripted.lock().unwrap().insert(sql.to_string(), rows);
    }

    /// The apps prepared so far, oldest first.
    pub fn prepared(&self) -> Vec<String> {
        self.prepared.lock().unwrap().clone()
    }

    /// Every statement so far, oldest first.
    pub fn ran(&self) -> Vec<Ran> {
        self.ran.lock().unwrap().clone()
    }

    fn record(&self, runner: Runner<'_>, schema: &str, sql: &str) {
        let app = match runner {
            Runner::Shell => None,
            Runner::App(manifest) => Some(manifest.id.clone()),
        };
        let row = (app, schema.to_string(), sql.to_string());
        self.ran.lock().unwrap().push(row);
    }
}

impl Database for StandIn {
    fn query<'a>(
        &'a self,
        runner: Runner<'a>,
        schema: &'a str,
        sql: &'a str,
        _: &'a [SqlValue],
    ) -> BoxFuture<'a, Result<Rows, WireError>> {
        self.record(runner, schema, sql);
        let rows = self.scripted.lock().unwrap().get(sql).cloned();
        Box::pin(async move { Ok(rows.unwrap_or_default()) })
    }

    fn execute<'a>(
        &'a self,
        runner: Runner<'a>,
        schema: &'a str,
        sql: &'a str,
        _: &'a [SqlValue],
    ) -> BoxFuture<'a, Result<u64, WireError>> {
        self.record(runner, schema, sql);
        let _ = self.written.send(());
        Box::pin(async { Ok(0) })
    }

    fn batch<'a>(
        &'a self,
        runner: Runner<'a>,
        schema: &'a str,
        statements: &'a [Statement],
    ) -> BoxFuture<'a, Result<Vec<u64>, WireError>> {
        for one in statements {
            self.record(runner, schema, &one.sql);
        }
        let counts = vec![0; statements.len()];
        let _ = self.written.send(());
        Box::pin(async move { Ok(counts) })
    }

    fn forget(&self, _: &str) {}

    fn prepare<'a>(&'a self, manifest: &'a Manifest) -> BoxFuture<'a, Result<(), WireError>> {
        self.prepared.lock().unwrap().push(manifest.id.clone());
        Box::pin(async { Ok(()) })
    }

    /// Ticks after every `execute` and `batch` (reads change nothing here).
    fn written(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.written.subscribe()
    }
}
