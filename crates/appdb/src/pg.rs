//! The apps' database on the Capsule's own server, over its Unix socket.
//!
//! One database holds every app's schemas. Each statement runs in a
//! transaction, on a connection logged in as the caller's role, with the
//! search path set to the chosen schema for that transaction only.
//! Placeholders use the extended protocol, so one call is one statement.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as SyncMutex};

use isonomic_proto::sql::{Column, Statement};
use isonomic_proto::{ErrorCode, Manifest, Rows, SqlValue, WireError};
use tokio::sync::{broadcast, Mutex, OnceCell};
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Config, NoTls, Transaction};

use crate::roles::{self, role_name, Setup};
use crate::values::{Cell, Param};
use crate::{quote, sql_error, BoxFuture, Database, Runner};

/// The database that holds every app's schemas.
pub const APPS_DATABASE: &str = "apps";

pub struct PgDatabase {
    socket_dir: PathBuf,
    port: u16,
    /// The engine's own role: the cluster's bootstrap superuser.
    admin: String,
    /// A database that always exists, to create the apps' one from.
    bootstrap: String,
    created: OnceCell<()>,
    clients: Mutex<HashMap<String, Arc<Mutex<Client>>>>,
    /// Apps whose setup is complete for their current manifest.
    ready: SyncMutex<HashSet<String>>,
    written: broadcast::Sender<()>,
}

/// The database's own message for an error, else the driver's.
pub(crate) fn message(error: &tokio_postgres::Error) -> String {
    if let Some(db) = error.as_db_error() {
        return db.message().to_string();
    }
    match std::error::Error::source(error) {
        Some(source) => format!("{error}: {source}"),
        None => error.to_string(),
    }
}

fn failure(error: tokio_postgres::Error) -> WireError {
    if error.as_db_error().is_none() && error.is_closed() {
        return WireError::new(ErrorCode::Internal, "the database is not available");
    }
    sql_error(message(&error))
}

fn unavailable(error: impl std::fmt::Display) -> WireError {
    WireError::new(
        ErrorCode::Internal,
        format!("the database is not available: {error}"),
    )
}

impl PgDatabase {
    /// The server listening in `socket_dir` (socket `.s.PGSQL.<port>`),
    /// reached as `admin`, the cluster's superuser, from `bootstrap`.
    pub fn new(socket_dir: PathBuf, port: u16, admin: &str, bootstrap: &str) -> Self {
        Self {
            socket_dir,
            port,
            admin: admin.to_string(),
            bootstrap: bootstrap.to_string(),
            created: OnceCell::new(),
            clients: Mutex::new(HashMap::new()),
            ready: SyncMutex::new(HashSet::new()),
            written: broadcast::channel(64).0,
        }
    }

    async fn connect(&self, role: &str, database: &str) -> Result<Client, WireError> {
        let mut config = Config::new();
        config
            .host_path(&self.socket_dir)
            .port(self.port)
            .user(role)
            .dbname(database)
            .application_name("isonomic-capsule");
        let (client, connection) = config.connect(NoTls).await.map_err(unavailable)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(client)
    }

    /// The apps' database exists and only owners reach `public`.
    async fn ensure_created(&self) -> Result<(), WireError> {
        self.created
            .get_or_try_init(|| async {
                let boot = self.connect(&self.admin, &self.bootstrap).await?;
                let found = boot
                    .query_opt(
                        "SELECT 1 FROM pg_database WHERE datname = $1",
                        &[&APPS_DATABASE],
                    )
                    .await
                    .map_err(failure)?;
                if found.is_none() {
                    let create = format!("CREATE DATABASE {}", quote(APPS_DATABASE));
                    boot.batch_execute(&create).await.map_err(failure)?;
                }
                let admin = self.client(&self.admin).await?;
                let admin = admin.lock().await;
                roles::lock_down(&admin, APPS_DATABASE)
                    .await
                    .map_err(failure)
            })
            .await
            .map(|_| ())
    }

    /// A connection logged in as `role`, opened again if it closed.
    async fn client(&self, role: &str) -> Result<Arc<Mutex<Client>>, WireError> {
        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get(role) {
            if !client.lock().await.is_closed() {
                return Ok(Arc::clone(client));
            }
        }
        let client = Arc::new(Mutex::new(self.connect(role, APPS_DATABASE).await?));
        clients.insert(role.to_string(), Arc::clone(&client));
        Ok(client)
    }

    /// The role a runner's statements run as, set up first if need be.
    async fn role_for(&self, runner: Runner<'_>) -> Result<String, WireError> {
        self.ensure_created().await?;
        let Runner::App(manifest) = runner else {
            return Ok(self.admin.clone());
        };
        let done = self.lock_ready().contains(&manifest.id);
        if !done {
            let admin = self.client(&self.admin).await?;
            let admin = admin.lock().await;
            match roles::provision(&admin, APPS_DATABASE, manifest).await {
                Ok(Setup::Complete) => {
                    self.lock_ready().insert(manifest.id.clone());
                }
                Ok(Setup::Waiting) => {}
                Err(message) => return Err(sql_error(message)),
            }
        }
        Ok(role_name(&manifest.id))
    }

    /// Commit, and tick `written` if the transaction wrote anything. The
    /// database knows: a transaction id is assigned only on a write.
    async fn commit(&self, tx: Transaction<'_>) -> Result<(), WireError> {
        let row = tx
            .query_one("SELECT txid_current_if_assigned() IS NOT NULL", &[])
            .await
            .map_err(failure)?;
        let wrote: bool = row.get(0);
        tx.commit().await.map_err(failure)?;
        if wrote {
            let _ = self.written.send(());
        }
        Ok(())
    }

    fn lock_ready(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.ready.lock().unwrap_or_else(|p| p.into_inner())
    }
}

async fn begin<'c>(client: &'c mut Client, schema: &str) -> Result<Transaction<'c>, WireError> {
    let tx = client.transaction().await.map_err(failure)?;
    let path = format!("SET LOCAL search_path TO {}", quote(schema));
    tx.batch_execute(&path).await.map_err(failure)?;
    Ok(tx)
}

/// Prepare one statement and check it takes as many parameters as given.
async fn prepare(
    tx: &Transaction<'_>,
    sql: &str,
    params: &[SqlValue],
) -> Result<tokio_postgres::Statement, WireError> {
    let statement = tx.prepare(sql).await.map_err(failure)?;
    if statement.params().len() != params.len() {
        return Err(sql_error(format!(
            "the statement takes {} parameters, {} were given",
            statement.params().len(),
            params.len()
        )));
    }
    Ok(statement)
}

fn refs(params: &[SqlValue]) -> Vec<Param<'_>> {
    params.iter().map(Param).collect()
}

fn dyn_refs<'a>(params: &'a [Param<'a>]) -> Vec<&'a (dyn ToSql + Sync)> {
    params.iter().map(|p| p as &(dyn ToSql + Sync)).collect()
}

impl Database for PgDatabase {
    fn query<'a>(
        &'a self,
        runner: Runner<'a>,
        schema: &'a str,
        sql: &'a str,
        params: &'a [SqlValue],
    ) -> BoxFuture<'a, Result<Rows, WireError>> {
        Box::pin(async move {
            let role = self.role_for(runner).await?;
            let client = self.client(&role).await?;
            let mut client = client.lock().await;
            let tx = begin(&mut client, schema).await?;
            let statement = prepare(&tx, sql, params).await?;
            let values = refs(params);
            let rows = tx
                .query(&statement, &dyn_refs(&values))
                .await
                .map_err(failure)?;
            let columns = statement
                .columns()
                .iter()
                .map(|c| Column {
                    name: c.name().to_string(),
                    kind: c.type_().name().to_string(),
                })
                .collect::<Vec<_>>();
            let mut out = Vec::with_capacity(rows.len());
            for row in &rows {
                let mut values = Vec::with_capacity(columns.len());
                for i in 0..columns.len() {
                    let Cell(value) = row.try_get(i).map_err(|e| sql_error(message(&e)))?;
                    values.push(value);
                }
                out.push(values);
            }
            self.commit(tx).await?;
            Ok(Rows { columns, rows: out })
        })
    }

    fn execute<'a>(
        &'a self,
        runner: Runner<'a>,
        schema: &'a str,
        sql: &'a str,
        params: &'a [SqlValue],
    ) -> BoxFuture<'a, Result<u64, WireError>> {
        Box::pin(async move {
            let role = self.role_for(runner).await?;
            let client = self.client(&role).await?;
            let mut client = client.lock().await;
            let tx = begin(&mut client, schema).await?;
            let statement = prepare(&tx, sql, params).await?;
            let values = refs(params);
            let n = tx
                .execute(&statement, &dyn_refs(&values))
                .await
                .map_err(failure)?;
            self.commit(tx).await?;
            Ok(n)
        })
    }

    fn batch<'a>(
        &'a self,
        runner: Runner<'a>,
        schema: &'a str,
        statements: &'a [Statement],
    ) -> BoxFuture<'a, Result<Vec<u64>, WireError>> {
        Box::pin(async move {
            let role = self.role_for(runner).await?;
            let client = self.client(&role).await?;
            let mut client = client.lock().await;
            let tx = begin(&mut client, schema).await?;
            let mut counts = Vec::with_capacity(statements.len());
            for one in statements {
                let statement = prepare(&tx, &one.sql, &one.params).await?;
                let values = refs(&one.params);
                let n = tx
                    .execute(&statement, &dyn_refs(&values))
                    .await
                    .map_err(failure)?;
                counts.push(n);
            }
            // Dropping the transaction on any error above rolls it back.
            self.commit(tx).await?;
            Ok(counts)
        })
    }

    fn forget(&self, app: &str) {
        self.lock_ready().remove(app);
    }

    fn written(&self) -> broadcast::Receiver<()> {
        self.written.subscribe()
    }

    fn prepare<'a>(&'a self, manifest: &'a Manifest) -> BoxFuture<'a, Result<(), WireError>> {
        Box::pin(async move { self.role_for(Runner::App(manifest)).await.map(|_| ()) })
    }
}
