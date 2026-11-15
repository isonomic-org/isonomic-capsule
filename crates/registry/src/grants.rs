//! Approved apps: each app's current manifest, and every token issued for
//! it. The registry never holds a token itself, only a digest of it, so a
//! copy of the database grants nothing.
//!
//! Granting an app again replaces its manifest and adds the new token's
//! digest; earlier digests stay, so tokens already handed out keep working.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

/// An app's approval as stored: its id and its manifest as JSON. The
/// registry does not read the manifest; the broker does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub app: String,
    pub manifest: String,
}

pub struct GrantStore {
    conn: Mutex<Connection>,
}

impl GrantStore {
    /// The grants in the registry database at `path`, beside the services.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::init(Connection::open(path)?)
    }

    /// In-memory grants (tests).
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS grants (
                app TEXT PRIMARY KEY,
                manifest TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS tokens (
                digest TEXT PRIMARY KEY,
                app TEXT NOT NULL REFERENCES grants(app)
            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Record an approval and the digest of the token issued for it, in one
    /// transaction. Returns once both are on disk.
    pub fn record(&self, grant: &Grant, token_digest: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock().expect("grants lock");
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO grants (app, manifest) VALUES (?1, ?2)
             ON CONFLICT(app) DO UPDATE SET manifest = excluded.manifest",
            params![grant.app, grant.manifest],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO tokens (digest, app) VALUES (?1, ?2)",
            params![token_digest, grant.app],
        )?;
        tx.commit()
    }

    /// The approval a token digest belongs to, with the app's current
    /// manifest; `None` for a token never issued.
    pub fn find(&self, token_digest: &str) -> rusqlite::Result<Option<Grant>> {
        let conn = self.conn.lock().expect("grants lock");
        conn.query_row(
            "SELECT g.app, g.manifest FROM tokens t JOIN grants g ON g.app = t.app
             WHERE t.digest = ?1",
            params![token_digest],
            |row| {
                Ok(Grant {
                    app: row.get(0)?,
                    manifest: row.get(1)?,
                })
            },
        )
        .optional()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(manifest: &str) -> Grant {
        Grant {
            app: "com.isonomic.hello".into(),
            manifest: manifest.into(),
        }
    }

    #[test]
    fn a_token_finds_its_app_and_the_current_manifest() {
        let store = GrantStore::in_memory().unwrap();
        assert_eq!(store.find("a").unwrap(), None);
        store.record(&grant("{\"v\":1}"), "a").unwrap();
        assert_eq!(store.find("a").unwrap(), Some(grant("{\"v\":1}")));

        // Granting again replaces the manifest and keeps the earlier token.
        store.record(&grant("{\"v\":2}"), "b").unwrap();
        assert_eq!(store.find("a").unwrap(), Some(grant("{\"v\":2}")));
        assert_eq!(store.find("b").unwrap(), Some(grant("{\"v\":2}")));
    }

    #[test]
    fn grants_survive_reopening_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.db");
        GrantStore::open(&path)
            .unwrap()
            .record(&grant("{}"), "a")
            .unwrap();
        let again = GrantStore::open(&path).unwrap();
        assert_eq!(again.find("a").unwrap(), Some(grant("{}")));
    }
}
