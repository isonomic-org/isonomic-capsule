//! A damaged `registry.db` must not stop the engine. Before anything opens
//! it, the engine runs SQLite's `quick_check`. A file that fails is moved,
//! never deleted, into the Capsule's `unreadable/` folder with a timestamp
//! (its `-wal` and `-shm` files beside it), and the engine starts with an
//! empty registry.
//!
//! What that costs the person, said plainly in [`SetAside::message`]: apps
//! must be approved again. Their data in the database and their files are
//! kept; the chats and the list of stored files from before stay in the
//! set-aside copy and are not shown.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

/// A registry that failed its check and was moved aside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAside {
    /// Where the damaged file went.
    pub to: PathBuf,
    /// What SQLite said.
    pub why: String,
}

impl SetAside {
    /// What the engine logs, in words a person can act on.
    pub fn message(&self) -> String {
        format!(
            "The list of approved apps could not be read ({}), so it was set aside in {} \
             and a new one was started. Apps must be approved again. Their data and files \
             are kept; chats from before are in the set-aside copy and are not shown.",
            self.why,
            self.to.display()
        )
    }
}

/// Why `db` fails SQLite's quick check, or `None` when it passes or does
/// not exist yet.
pub fn damage(db: &Path) -> Option<String> {
    if !db.exists() {
        return None;
    }
    // Read-only and immutable: the check writes nothing, and leaves the
    // `-wal` and `-shm` files exactly as they were. The main file, as of its
    // last checkpoint, is consistent on its own.
    let uri = format!("file:{}?mode=ro&immutable=1", uri_path(db));
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let conn = match Connection::open_with_flags(uri, flags) {
        Ok(conn) => conn,
        Err(e) => return Some(e.to_string()),
    };
    let rows: Result<Vec<String>, _> = conn
        .prepare("PRAGMA quick_check")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect());
    match rows {
        Ok(rows) if rows.len() == 1 && rows[0] == "ok" => None,
        Ok(rows) => Some(rows.into_iter().take(3).collect::<Vec<_>>().join("; ")),
        Err(e) => Some(e.to_string()),
    }
}

/// `path` for a SQLite URI: `%`, `?` and `#` escaped, the rest as it is.
fn uri_path(path: &Path) -> String {
    let mut out = String::new();
    for c in path.to_string_lossy().chars() {
        match c {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            c => out.push(c),
        }
    }
    out
}

/// Check `db`; if it is damaged, move it (and its `-wal` and `-shm`) into
/// `unreadable` as `registry-<stamp>.db`.
pub fn check_or_set_aside(
    db: &Path,
    unreadable: &Path,
    stamp: &str,
) -> std::io::Result<Option<SetAside>> {
    let Some(why) = damage(db) else {
        return Ok(None);
    };
    std::fs::create_dir_all(unreadable)?;
    let to = unreadable.join(format!("registry-{stamp}.db"));
    for suffix in ["", "-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{suffix}", db.display()));
        if from.exists() {
            std::fs::rename(&from, format!("{}{suffix}", to.display()))?;
        }
    }
    Ok(Some(SetAside { to, why }))
}

/// A UTC timestamp for a file name: `20261116T071508Z`.
pub fn stamp(unix_secs: u64) -> String {
    let (days, secs) = (unix_secs / 86_400, unix_secs % 86_400);
    let (y, m, d) = civil(days as i64);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Days since 1970-01-01 as a calendar date (proleptic Gregorian).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_healthy_or_missing_registry_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("registry.db");
        assert_eq!(
            check_or_set_aside(&db, &dir.path().join("unreadable"), "x").unwrap(),
            None
        );
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE grants (app TEXT); INSERT INTO grants VALUES ('a');")
            .unwrap();
        drop(conn);
        std::fs::write(dir.path().join("registry.db-wal"), b"").unwrap();
        assert_eq!(
            check_or_set_aside(&db, &dir.path().join("unreadable"), "x").unwrap(),
            None
        );
        assert!(db.exists());
        assert!(
            dir.path().join("registry.db-wal").exists(),
            "the check touches nothing"
        );
    }

    #[test]
    fn a_damaged_registry_is_moved_aside_whole_and_never_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("registry.db");
        let garbage = b"this was a registry before the disk failed".repeat(200);
        std::fs::write(&db, &garbage).unwrap();
        std::fs::write(dir.path().join("registry.db-wal"), b"wal").unwrap();
        let unreadable = dir.path().join("unreadable");
        let set = check_or_set_aside(&db, &unreadable, "20261116T071508Z")
            .unwrap()
            .expect("set aside");
        assert!(!db.exists());
        assert_eq!(set.to, unreadable.join("registry-20261116T071508Z.db"));
        assert_eq!(
            std::fs::read(&set.to).unwrap(),
            garbage,
            "kept byte for byte"
        );
        assert!(unreadable.join("registry-20261116T071508Z.db-wal").exists());
        let message = set.message();
        assert!(message.contains("Apps must be approved again"), "{message}");
        assert!(message.contains("data and files are kept"), "{message}");
    }

    #[test]
    fn a_path_with_spaces_and_marks_is_checked_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Application Support #1 100%");
        std::fs::create_dir_all(&folder).unwrap();
        let db = folder.join("registry.db");
        Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE t (x)")
            .unwrap();
        assert_eq!(damage(&db), None);
        std::fs::write(&db, b"not a database at all".repeat(100)).unwrap();
        assert!(damage(&db).is_some());
    }

    #[test]
    fn stamps_read_as_utc() {
        assert_eq!(stamp(0), "19700101T000000Z");
        assert_eq!(stamp(1_794_813_308), "20261116T071508Z");
        assert_eq!(stamp(951_782_400), "20000229T000000Z");
    }
}
