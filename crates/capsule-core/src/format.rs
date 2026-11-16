//! What a Capsule's data is, so a later build opens what an earlier one
//! left (see `docs/capsule-format.md`).
//!
//! `capsule.json` at the root holds the format number. The engine
//! checks it before touching anything else:
//!
//! - **No marker:** a new Capsule, or one from a build before markers
//!   (every one of those is format 1). The marker is written.
//! - **The same format:** opened, and `last-opened-by` is updated.
//! - **An older format:** its migrations run, oldest first, then the
//!   marker moves on. There are none yet: format 1 is the first.
//! - **A newer format:** refused, and nothing is changed, so the newer
//!   build can still open it.
//!
//! The bundled database's data directory records its own major version
//! (`PG_VERSION`); one that is not this build's is refused too, since a
//! major version cannot read another's files.

use std::path::Path;

use crate::paths::CapsulePaths;

/// The Capsule format this build writes and reads.
pub const CAPSULE_FORMAT: u32 = 1;

/// The bundled database's major version (`sidecars.json`: 18.4.0).
pub const DATABASE_MAJOR: u32 = 18;

/// What the marker says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub format: u32,
    /// The engine version that made the Capsule; `unknown` for one from
    /// before markers.
    pub created_by: String,
    pub last_opened_by: String,
    /// 32 random hex characters, made once and kept, so what the Capsule
    /// keeps outside its folder (keychain items for its Isonomic ID) is
    /// labelled for it, wherever the folder moves. Added without a format
    /// change: older builds ignore it.
    pub capsule_id: String,
    /// A throwaway Capsule, such as the self-check's: set by whoever made
    /// the folder (`"scratch": true`), never by the engine, and kept on
    /// every open. Only a scratch Capsule may keep its ID's keys in memory
    /// (`ISONOMIC_KEY_STORE=memory`), so a real one never ends up with a key
    /// log whose keys are gone.
    pub scratch: bool,
}

/// Why a Capsule cannot be opened, in words a person can act on.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FormatError {
    #[error(
        "This Isonomic data was made by a newer version of Isonomic (data format {found}; \
         this version reads up to {CAPSULE_FORMAT}). Update Isonomic to open it. Nothing was changed."
    )]
    Newer { found: u32 },
    #[error(
        "This Isonomic data uses database version {found}, and this version of Isonomic \
         uses {DATABASE_MAJOR}. Use the version of Isonomic that made it. Nothing was changed."
    )]
    Database { found: u32 },
    #[error("The Isonomic data marker {path} cannot be read: {why}")]
    Unreadable { path: String, why: String },
    #[error("The Isonomic data marker cannot be written: {0}")]
    Io(String),
}

impl Marker {
    fn render(&self) -> String {
        let scratch = if self.scratch {
            ",\n  \"scratch\": true"
        } else {
            ""
        };
        format!(
            "{{\n  \"format\": {},\n  \"capsule_id\": \"{}\",\n  \"created_by\": \"{}\",\n  \"last_opened_by\": \"{}\"{scratch}\n}}\n",
            self.format,
            escape(&self.capsule_id),
            escape(&self.created_by),
            escape(&self.last_opened_by)
        )
    }

    /// Read the marker's three fields. Only what this module writes is
    /// expected; anything else in the file is ignored.
    fn parse(text: &str) -> Option<Self> {
        let field = |name: &str| {
            let key = format!("\"{name}\":");
            let rest = text[text.find(&key)? + key.len()..].trim_start();
            let end = rest.find([',', '\n', '}']).unwrap_or(rest.len());
            Some(rest[..end].trim().trim_matches('"').to_string())
        };
        Some(Self {
            format: field("format")?.parse().ok()?,
            created_by: field("created_by").unwrap_or_else(|| "unknown".into()),
            last_opened_by: field("last_opened_by").unwrap_or_else(|| "unknown".into()),
            // Empty when missing: `open` makes one.
            capsule_id: field("capsule_id")
                .filter(|id| is_capsule_id(id))
                .unwrap_or_default(),
            scratch: field("scratch").is_some_and(|v| v == "true"),
        })
    }
}

fn is_capsule_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 16 random bytes in hexadecimal, from the system's generator.
fn new_capsule_id() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn escape(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
        .collect()
}

/// Check the Capsule at `paths` before the engine opens its data, and
/// record that `engine` opened it.
pub fn open(paths: &CapsulePaths, engine: &str) -> Result<Marker, FormatError> {
    let path = paths.capsule_marker();
    let found = match std::fs::read_to_string(&path) {
        Ok(text) => Some(Marker::parse(&text).ok_or_else(|| FormatError::Unreadable {
            path: path.display().to_string(),
            why: "it is not a marker this version understands".into(),
        })?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(FormatError::Unreadable {
                path: path.display().to_string(),
                why: e.to_string(),
            })
        }
    };
    if let Some(m) = &found {
        if m.format > CAPSULE_FORMAT {
            return Err(FormatError::Newer { found: m.format });
        }
    }
    if let Some(major) = database_major(&paths.service_dir("postgres")) {
        if major != DATABASE_MAJOR {
            return Err(FormatError::Database { found: major });
        }
    }
    // Format 1 is the first: an older one has nothing to migrate from yet.
    let marker = Marker {
        format: CAPSULE_FORMAT,
        created_by: found
            .as_ref()
            .map_or_else(|| first_maker(paths, engine), |m| m.created_by.clone()),
        last_opened_by: engine.to_string(),
        capsule_id: match found.as_ref().map(|m| m.capsule_id.clone()) {
            Some(id) if !id.is_empty() => id,
            _ => new_capsule_id().map_err(|e| FormatError::Io(e.to_string()))?,
        },
        scratch: found.as_ref().is_some_and(|m| m.scratch),
    };
    write(&path, &marker.render()).map_err(|e| FormatError::Io(e.to_string()))?;
    Ok(marker)
}

/// Who made a Capsule that has no marker: this engine when it is new,
/// else an earlier build that wrote none.
fn first_maker(paths: &CapsulePaths, engine: &str) -> String {
    match paths.registry_db().exists() {
        true => "unknown".into(),
        false => engine.into(),
    }
}

/// The major version a database data directory was made with.
fn database_major(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join("PG_VERSION"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = path.with_extension("json.part");
    std::fs::write(&part, text)?;
    std::fs::rename(&part, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capsule() -> (tempfile::TempDir, CapsulePaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = CapsulePaths::new(dir.path());
        (dir, paths)
    }

    #[test]
    fn a_scratch_capsule_stays_scratch_and_a_real_one_never_becomes_one() {
        let (_dir, paths) = capsule();
        // As the self-check writes it, before the engine first opens it.
        std::fs::write(
            paths.capsule_marker(),
            "{\"format\": 1, \"scratch\": true}\n",
        )
        .unwrap();
        let first = open(&paths, "0.9.0").unwrap();
        assert!(first.scratch);
        assert!(is_capsule_id(&first.capsule_id));
        let again = open(&paths, "0.9.0").unwrap();
        assert!(again.scratch, "kept when the marker is written again");
        assert_eq!(again.capsule_id, first.capsule_id);

        let (_real, real) = capsule();
        assert!(!open(&real, "0.9.0").unwrap().scratch);
        let text = std::fs::read_to_string(real.capsule_marker()).unwrap();
        assert!(!text.contains("scratch"), "{text}");
    }

    #[test]
    fn a_new_capsule_gets_a_marker_naming_its_maker() {
        let (_dir, paths) = capsule();
        let marker = open(&paths, "0.9.0").unwrap();
        assert_eq!((marker.format, marker.created_by.as_str()), (1, "0.9.0"));
        let again = open(&paths, "1.0.0").unwrap();
        assert_eq!(again.created_by, "0.9.0");
        assert!(is_capsule_id(&marker.capsule_id), "{}", marker.capsule_id);
        assert_eq!(again.capsule_id, marker.capsule_id, "made once and kept");
        assert_eq!(again.last_opened_by, "1.0.0");
    }

    #[test]
    fn a_capsule_from_before_markers_is_format_one() {
        let (_dir, paths) = capsule();
        std::fs::write(paths.registry_db(), b"").unwrap();
        let marker = open(&paths, "1.0.0").unwrap();
        assert_eq!((marker.format, marker.created_by.as_str()), (1, "unknown"));
        assert!(is_capsule_id(&marker.capsule_id), "one is added");
        assert!(paths.capsule_marker().exists());
    }

    #[test]
    fn a_newer_capsule_is_refused_and_left_alone() {
        let (_dir, paths) = capsule();
        let text = "{\n  \"format\": 7,\n  \"created_by\": \"3.0.0\",\n  \"last_opened_by\": \"3.0.0\"\n}\n";
        std::fs::write(paths.capsule_marker(), text).unwrap();
        let refused = open(&paths, "1.0.0").unwrap_err();
        assert_eq!(refused, FormatError::Newer { found: 7 });
        assert!(refused.to_string().contains("Update Isonomic to open it"));
        assert_eq!(
            std::fs::read_to_string(paths.capsule_marker()).unwrap(),
            text
        );
    }

    #[test]
    fn another_database_major_is_refused() {
        let (_dir, paths) = capsule();
        let db = paths.service_dir("postgres");
        std::fs::create_dir_all(&db).unwrap();
        std::fs::write(db.join("PG_VERSION"), "17\n").unwrap();
        assert_eq!(
            open(&paths, "1.0.0").unwrap_err(),
            FormatError::Database { found: 17 }
        );
        assert!(!paths.capsule_marker().exists(), "nothing was written");
        std::fs::write(db.join("PG_VERSION"), format!("{DATABASE_MAJOR}\n")).unwrap();
        open(&paths, "1.0.0").unwrap();
    }

    #[test]
    fn a_marker_that_is_not_one_is_refused() {
        let (_dir, paths) = capsule();
        std::fs::write(paths.capsule_marker(), "hello").unwrap();
        assert!(matches!(
            open(&paths, "1.0.0"),
            Err(FormatError::Unreadable { .. })
        ));
    }
}
