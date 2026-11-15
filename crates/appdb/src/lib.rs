//! Apps' databases (SDK request R5): the engine side of the `sql`
//! interface, below the capability check.
//!
//! The broker decides which schema a call runs in (`proto::auth::
//! authorize_schema`) and hands it here with who is running it. The real
//! guard is the database's own: each app has a login role that owns its
//! schemas and may only read the schemas its manifest lists, and its
//! statements run on a connection logged in as that role, so no statement
//! can step outside it. The shell runs as the engine's own role.
//!
//! [`PgDatabase`] is the real database over the Capsule's Unix socket;
//! [`StandIn`] answers without one, for hermetic tests of the layers above.

mod pg;
mod roles;
mod standin;
mod values;

use std::future::Future;
use std::pin::Pin;

use isonomic_proto::sql::Statement;
use isonomic_proto::{ErrorCode, Manifest, Rows, SqlValue, WireError};

pub use pg::PgDatabase;
pub use roles::role_name;
pub use standin::StandIn;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Who a statement runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner<'a> {
    /// The window, as the engine's own role.
    Shell,
    /// An app, as its own role. Its manifest says what that role may do.
    App(&'a Manifest),
}

/// What the broker needs from the apps' database.
pub trait Database: Send + Sync + 'static {
    /// Run one read, in `schema`.
    fn query<'a>(
        &'a self,
        runner: Runner<'a>,
        schema: &'a str,
        sql: &'a str,
        params: &'a [SqlValue],
    ) -> BoxFuture<'a, Result<Rows, WireError>>;

    /// Run one write, in `schema`; how many rows it changed.
    fn execute<'a>(
        &'a self,
        runner: Runner<'a>,
        schema: &'a str,
        sql: &'a str,
        params: &'a [SqlValue],
    ) -> BoxFuture<'a, Result<u64, WireError>>;

    /// Run every statement in one transaction: all or none.
    fn batch<'a>(
        &'a self,
        runner: Runner<'a>,
        schema: &'a str,
        statements: &'a [Statement],
    ) -> BoxFuture<'a, Result<Vec<u64>, WireError>>;

    /// An app's approval changed: its role, schemas and read access are
    /// set up again before its next statement.
    fn forget(&self, app: &str);

    /// Ticks after every committed transaction that wrote, whichever
    /// method or engine path ran it.
    fn written(&self) -> tokio::sync::broadcast::Receiver<()>;

    /// Set an app's role, schemas and read access up now, as its approval
    /// stands. Statements do it themselves when it has not happened, so a
    /// failure here (the database still starting) costs nothing.
    fn prepare<'a>(&'a self, manifest: &'a Manifest) -> BoxFuture<'a, Result<(), WireError>>;
}

/// The database refused, with its own message.
pub fn sql_error(message: impl Into<String>) -> WireError {
    WireError::new(ErrorCode::SqlError, message)
}

/// Quote an identifier for the database: double quotes, with any double
/// quote inside doubled.
pub fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
