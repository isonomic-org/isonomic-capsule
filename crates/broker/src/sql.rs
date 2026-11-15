//! The `sql` interface (SDK request R5) above the apps' database: which
//! schema a call runs in, live queries, and the frame limit.
//!
//! `proto::auth::authorize_schema` picks the schema and refuses what the
//! manifest does not allow; the database's roles enforce it again. A live
//! query is run again after every committed transaction that wrote, through
//! any method or engine path that shares the database, and its subscriber
//! is sent the whole new result only when it changed.

use std::sync::Arc;

use appdb::{sql_error, Database, Runner};
use isonomic_proto::auth::authorize_schema;
use isonomic_proto::{
    Call, ErrorCode, Event, Manifest, Reply, Rows, ServerMessage, SqlValue, WireError,
    MAX_FRAME_BYTES,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// Room left in a frame for everything around the rows.
const FRAME_MARGIN: usize = 4096;

pub struct Sql {
    db: Arc<dyn Database>,
}

/// What a sql call leads to.
pub enum Answer {
    Reply(Reply),
    /// `sql.subscribe`: the first result, and the query to keep running.
    Live(Rows, Box<LiveQuery>),
}

fn runner(manifest: Option<&Manifest>) -> Runner<'_> {
    manifest.map_or(Runner::Shell, Runner::App)
}

/// A result must fit in one frame; apps page larger ones with `limit`.
fn fits(rows: Rows) -> Result<Rows, WireError> {
    let size = serde_json::to_vec(&rows).map_or(usize::MAX, |b| b.len());
    if size > MAX_FRAME_BYTES - FRAME_MARGIN {
        return Err(sql_error(format!(
            "the result is {size} bytes, more than one message holds; page it with limit"
        )));
    }
    Ok(rows)
}

impl Sql {
    pub fn new(db: Arc<dyn Database>) -> Self {
        Self { db }
    }

    /// An app's approval changed: set its database access up again.
    pub fn granted(&self, app: &str) {
        self.db.forget(app);
    }

    /// Answer one sql call for `manifest` (`None` is the shell). The caller
    /// has already run `proto::auth::authorize`.
    pub async fn answer(
        &self,
        manifest: Option<&Manifest>,
        call: Call,
    ) -> Result<Answer, WireError> {
        let who = runner(manifest);
        match call {
            Call::SqlQuery {
                sql,
                params,
                schema,
            } => {
                let schema = authorize_schema(manifest, schema.as_deref(), false)?;
                let rows = self.db.query(who, &schema, &sql, &params).await?;
                Ok(Answer::Reply(Reply::Rows(fits(rows)?)))
            }
            Call::SqlExecute {
                sql,
                params,
                schema,
            } => {
                let schema = authorize_schema(manifest, schema.as_deref(), true)?;
                let rows_affected = self.db.execute(who, &schema, &sql, &params).await?;
                Ok(Answer::Reply(Reply::Executed { rows_affected }))
            }
            Call::SqlBatch { statements, schema } => {
                let schema = authorize_schema(manifest, schema.as_deref(), true)?;
                let rows_affected = self.db.batch(who, &schema, &statements).await?;
                Ok(Answer::Reply(Reply::ExecutedBatch { rows_affected }))
            }
            Call::SqlSubscribe {
                sql,
                params,
                schema,
            } => {
                let schema = authorize_schema(manifest, schema.as_deref(), false)?;
                // Listen before the first run, so no write falls between.
                let written = self.db.written();
                let rows = fits(self.db.query(who, &schema, &sql, &params).await?)?;
                let live = LiveQuery {
                    db: Arc::clone(&self.db),
                    manifest: manifest.cloned(),
                    schema,
                    sql,
                    params,
                    last: rows.clone(),
                    written,
                };
                Ok(Answer::Live(rows, Box::new(live)))
            }
            other => Err(WireError::new(
                ErrorCode::UnknownMethod,
                format!("{} is not a sql method", other.method()),
            )),
        }
    }
}

/// A live query between its first result and its forwarding task.
pub struct LiveQuery {
    db: Arc<dyn Database>,
    manifest: Option<Manifest>,
    schema: String,
    sql: String,
    params: Vec<SqlValue>,
    last: Rows,
    written: broadcast::Receiver<()>,
}

impl LiveQuery {
    /// Run the query again after each write and send `rows_changed` on
    /// `subscription` when the result differs, until either side goes away.
    pub fn start(self, subscription: u64, outbox: mpsc::Sender<ServerMessage>) -> JoinHandle<()> {
        let LiveQuery {
            db,
            manifest,
            schema,
            sql,
            params,
            mut last,
            mut written,
        } = self;
        tokio::spawn(async move {
            loop {
                match written.recv().await {
                    // Behind is fine: one run catches up with every write.
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
                let who = runner(manifest.as_ref());
                // A failed run (the database restarting, say) sends nothing;
                // the next write tries again.
                let Ok(rows) = db.query(who, &schema, &sql, &params).await else {
                    continue;
                };
                let Ok(rows) = fits(rows) else { continue };
                if rows == last {
                    continue;
                }
                last = rows.clone();
                let message = ServerMessage::Event {
                    subscription,
                    event: Event::RowsChanged(rows),
                };
                if outbox.send(message).await.is_err() {
                    return;
                }
            }
        })
    }
}
