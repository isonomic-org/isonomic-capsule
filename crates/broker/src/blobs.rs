//! The `blobs` interface (SDK request R3): byte objects an app stores and
//! reads back by handle.
//!
//! Each blob is a file in the Capsule's blob directory, named by its
//! handle, with a row in the registry database: owner, content type, size
//! and bytes written. A write's bytes are synced to disk before the row
//! counts them, and the row is committed before the answer, so a client
//! that saw `written: n` can resume at `n` after a restart. Bytes past the
//! row's count (a write cut short by a crash) are dropped at the next
//! write. Handles are random, never derived from content.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use isonomic_proto::auth::too_large;
use isonomic_proto::blobs::CHUNK_BYTES;
use isonomic_proto::{BlobInfo, Bytes, Call, ErrorCode, Reply, WireError};
use rusqlite::{params, Connection, OptionalExtension};

/// The engine's own limit on one blob, on top of each manifest's: 64 MiB.
pub const DEFAULT_LIMIT: u64 = 64 * 1024 * 1024;

pub struct Blobs {
    dir: PathBuf,
    conn: Mutex<Connection>,
    limit: u64,
}

struct Row {
    owner: Option<String>,
    info: BlobInfo,
}

fn internal(error: impl std::fmt::Display) -> WireError {
    WireError::new(ErrorCode::Internal, format!("blob store: {error}"))
}

fn unknown(handle: &str) -> WireError {
    WireError::new(ErrorCode::UnknownBlob, format!("no blob {handle}"))
}

impl Blobs {
    /// Blobs in `dir`, with their rows in the database at `db`.
    pub fn open(dir: &Path, db: &Path) -> Result<Self, WireError> {
        std::fs::create_dir_all(dir).map_err(internal)?;
        {
            use std::os::unix::fs::PermissionsExt;
            let user_only = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(dir, user_only).map_err(internal)?;
        }
        let conn = Connection::open(db).map_err(internal)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(internal)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS blobs (
                handle TEXT PRIMARY KEY,
                owner TEXT,
                content_type TEXT NOT NULL,
                size INTEGER NOT NULL,
                written INTEGER NOT NULL
            );",
        )
        .map_err(internal)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            conn: Mutex::new(conn),
            limit: DEFAULT_LIMIT,
        })
    }

    /// Replace the engine's own limit (tests).
    pub fn with_limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn path(&self, handle: &str) -> PathBuf {
        self.dir.join(handle)
    }

    /// Answer one blobs call for `app` (`None` is the shell). The caller
    /// has already run `proto::auth::authorize`. Blocking: file and
    /// database work, so call it off the async runtime.
    pub fn answer(&self, app: Option<&str>, call: Call) -> Result<Reply, WireError> {
        // One lock for the database and the files keeps each blob's writes
        // in order.
        let conn = self.lock();
        match call {
            Call::BlobsPut { content_type, data } => {
                let size = data.0.len() as u64;
                let max = self.limit.min(CHUNK_BYTES);
                if size > max {
                    return Err(too_large(size, max));
                }
                let info = self.create(&conn, app, content_type, size)?;
                self.write(&conn, app, &info.handle, 0, data)
                    .map(Reply::Blob)
            }
            Call::BlobsCreate { size, .. } if size > self.limit => Err(too_large(size, self.limit)),
            Call::BlobsCreate { content_type, size } => {
                self.create(&conn, app, content_type, size).map(Reply::Blob)
            }
            Call::BlobsWrite {
                handle,
                offset,
                data,
            } => self
                .write(&conn, app, &handle, offset, data)
                .map(Reply::Blob),
            Call::BlobsGet {
                handle,
                offset,
                length,
            } => self.get(&conn, app, &handle, offset, length),
            Call::BlobsInfo { handle } => Ok(Reply::Blob(mine(&conn, app, &handle)?.info)),
            Call::BlobsDelete { handle } => {
                mine(&conn, app, &handle)?;
                conn.execute("DELETE FROM blobs WHERE handle = ?1", params![handle])
                    .map_err(internal)?;
                match std::fs::remove_file(self.path(&handle)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(internal(e)),
                    _ => {}
                }
                Ok(Reply::Deleted {})
            }
            other => Err(WireError::new(
                ErrorCode::UnknownMethod,
                format!("{} is not a blobs method", other.method()),
            )),
        }
    }

    fn create(
        &self,
        conn: &Connection,
        app: Option<&str>,
        content_type: String,
        size: u64,
    ) -> Result<BlobInfo, WireError> {
        let handle = format!(
            "blob-{}",
            crate::random::hex(&crate::random::bytes::<16>().map_err(internal)?)
        );
        File::create(self.path(&handle))
            .and_then(|f| f.sync_all())
            .map_err(internal)?;
        conn.execute(
            "INSERT INTO blobs (handle, owner, content_type, size, written)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![handle, app, content_type, size],
        )
        .map_err(internal)?;
        Ok(BlobInfo {
            handle,
            content_type,
            size,
            written: 0,
        })
    }

    fn write(
        &self,
        conn: &Connection,
        app: Option<&str>,
        handle: &str,
        offset: u64,
        data: Bytes,
    ) -> Result<BlobInfo, WireError> {
        let mut info = mine(conn, app, handle)?.info;
        if offset != info.written {
            let message = format!("{handle} expects offset {}", info.written);
            return Err(WireError::new(ErrorCode::BadRequest, message));
        }
        let end = offset + data.0.len() as u64;
        if data.0.len() as u64 > CHUNK_BYTES || end > info.size {
            return Err(too_large(end, info.size.min(offset + CHUNK_BYTES)));
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path(handle))
            .map_err(internal)?;
        // Drop any bytes a crash left past the count, then append.
        file.set_len(offset).map_err(internal)?;
        file.seek(SeekFrom::Start(offset)).map_err(internal)?;
        file.write_all(&data.0).map_err(internal)?;
        file.sync_data().map_err(internal)?;
        conn.execute(
            "UPDATE blobs SET written = ?2 WHERE handle = ?1",
            params![handle, end],
        )
        .map_err(internal)?;
        info.written = end;
        Ok(info)
    }

    fn get(
        &self,
        conn: &Connection,
        app: Option<&str>,
        handle: &str,
        offset: u64,
        length: Option<u64>,
    ) -> Result<Reply, WireError> {
        let info = mine(conn, app, handle)?.info;
        if !info.complete() {
            let message = format!("{handle} has {} of {} bytes", info.written, info.size);
            return Err(WireError::new(ErrorCode::Incomplete, message));
        }
        let start = offset.min(info.size);
        let take = length.unwrap_or(CHUNK_BYTES).min(CHUNK_BYTES);
        let end = (start + take).min(info.size);
        let mut data = vec![0u8; (end - start) as usize];
        let mut file = File::open(self.path(handle)).map_err(internal)?;
        file.seek(SeekFrom::Start(start)).map_err(internal)?;
        file.read_exact(&mut data).map_err(internal)?;
        Ok(Reply::BlobData {
            info,
            offset: start,
            data: Bytes(data),
        })
    }
}

/// The caller's blob. Another app's handle answers exactly as an unknown
/// one; the shell reaches every blob.
fn mine(conn: &Connection, app: Option<&str>, handle: &str) -> Result<Row, WireError> {
    let row = conn
        .query_row(
            "SELECT owner, content_type, size, written FROM blobs WHERE handle = ?1",
            params![handle],
            |row| {
                Ok(Row {
                    owner: row.get(0)?,
                    info: BlobInfo {
                        handle: handle.to_string(),
                        content_type: row.get(1)?,
                        size: row.get(2)?,
                        written: row.get(3)?,
                    },
                })
            },
        )
        .optional()
        .map_err(internal)?;
    match row {
        Some(row) if app.is_none() || row.owner.as_deref() == app => Ok(row),
        _ => Err(unknown(handle)),
    }
}

#[cfg(test)]
#[path = "blobs_tests.rs"]
mod tests;
