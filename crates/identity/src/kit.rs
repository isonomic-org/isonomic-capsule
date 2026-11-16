//! The recovery kit: what lets a person who loses their Mac have the same
//! Isonomic ID on a new one. Local only: the kit is a document the window
//! saves as a file and prints; nothing goes to a network.
//!
//! **The document** (`isonomic-recovery-kit`, version 1) is JSON: the ID,
//! when it was made, the recovery seed as 24 words ([`crate::words`]), the
//! person's key log as it stands (each signed entry in hexadecimal, as
//! `identity/person.log` keeps it), and `check`, the SHA-256 of the
//! document without it, so a damaged copy is refused rather than half read.
//!
//! **Export** reads the seed from the key store and gives the kit to the
//! shell only, when the person asked; the engine logs that it happened,
//! never the seed.
//!
//! **Restore** takes the document and the words (typed from paper): the
//! document verifies, the words' checksum holds, and the recovery key they
//! derive is the one the log's last entry commits to. Then the log is
//! adopted and one entry signed by that recovery key retires every old
//! signing key and commits new keys made in this Mac's key store
//! ([`crate::Person`]). The ID does not change. A rotation key is used once,
//! so the kit is then spent: the restore makes a new seed and gives back a
//! new kit, which the person must save instead.
//!
//! **An older kit**, exported before a later device rotation, still
//! restores: device rotations cannot change the recovery commitment. Its
//! recovery entry then parts ways with the lost Mac's longer log. The
//! identity library never resolves a fork: here the restored log is the one
//! known, and a copy of the longer log arriving later is kept apart in
//! `identity/forks/` and never adopted.

use std::path::{Path, PathBuf};

use id_core::log::{Log, Role};
use id_core::Signed;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::files::{hex, unhex, write_whole};
use crate::recovery::recovery_public_key;
use crate::words;

/// What the document calls itself.
pub const FORMAT: &str = "isonomic-recovery-kit";
/// The document version this build writes and reads.
pub const VERSION: u64 = 1;

/// A recovery kit, before or after it is written as a document.
#[derive(Debug, Clone)]
pub struct Kit {
    pub id: String,
    pub made: u64,
    pub words: Vec<String>,
    pub log: Vec<Signed>,
}

/// Why a kit or its words were refused, in words a person can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KitError {
    #[error("This file is not an Isonomic recovery kit.")]
    NotAKit,
    #[error(
        "This recovery kit was made by a newer version of Isonomic. Update Isonomic to use it."
    )]
    Newer,
    #[error("This recovery kit is damaged: its contents don't match its check. Use another copy.")]
    Damaged,
    #[error("This recovery kit's record of the ID doesn't verify ({0}). Use another copy.")]
    Log(String),
    #[error("{0}. Check the words against your recovery kit.")]
    Words(String),
    #[error("These words are not this recovery kit's words.")]
    NotThisKit,
    #[error(
        "This recovery kit can no longer restore the ID: it was used, or replaced, since it \
         was made. Use your newest kit."
    )]
    Spent,
    #[error("This Mac already has an Isonomic ID with apps or chats. Nothing was changed.")]
    InUse,
    #[error("This Mac already has an Isonomic ID. Nothing was changed.")]
    HasId,
}

fn body(kit: &Kit) -> Value {
    let log: Vec<String> = kit
        .log
        .iter()
        .map(|e| hex(&e.to_bytes().unwrap_or_default()))
        .collect();
    json!({
        "format": FORMAT,
        "version": VERSION,
        "id": kit.id,
        "made": kit.made,
        "words": kit.words,
        "log": log,
    })
}

fn digest(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

impl Kit {
    /// The document, with its check.
    pub fn to_document(&self) -> String {
        let mut doc = body(self);
        let check = digest(&doc.to_string());
        doc["check"] = json!(check);
        serde_json::to_string_pretty(&doc).expect("a kit serializes")
    }

    /// The kit a document holds, if it is whole and its log verifies.
    pub fn from_document(text: &str) -> Result<Kit, KitError> {
        let mut doc: Value = serde_json::from_str(text).map_err(|_| KitError::NotAKit)?;
        if doc["format"] != FORMAT {
            return Err(KitError::NotAKit);
        }
        match doc["version"].as_u64() {
            Some(VERSION) => {}
            Some(v) if v > VERSION => return Err(KitError::Newer),
            _ => return Err(KitError::NotAKit),
        }
        let check = doc
            .as_object_mut()
            .and_then(|o| o.remove("check"))
            .and_then(|c| c.as_str().map(String::from))
            .ok_or(KitError::Damaged)?;
        if digest(&doc.to_string()) != check {
            return Err(KitError::Damaged);
        }
        let strings = |key: &str| -> Option<Vec<String>> {
            doc[key]
                .as_array()?
                .iter()
                .map(|v| v.as_str().map(String::from))
                .collect()
        };
        let log = strings("log")
            .ok_or(KitError::Damaged)?
            .iter()
            .map(|h| unhex(h).and_then(|b| Signed::from_bytes(&b).ok()))
            .collect::<Option<Vec<_>>>()
            .ok_or(KitError::Damaged)?;
        let kit = Kit {
            id: doc["id"].as_str().ok_or(KitError::Damaged)?.to_string(),
            made: doc["made"].as_u64().ok_or(KitError::Damaged)?,
            words: strings("words").unwrap_or_default(),
            log,
        };
        let opened = kit.open()?;
        if opened.id().to_string() != kit.id {
            return Err(KitError::Damaged);
        }
        Ok(kit)
    }

    /// The kit's log, verified from its first entry.
    pub fn open(&self) -> Result<Log, KitError> {
        Log::open(self.log.clone(), &crate::directory::checks())
            .map_err(|e| KitError::Log(format!("{e:?}")))
    }
}

/// The seed `words` spell, if it is the recovery key `log` commits to now.
pub fn seed_for(log: &Log, words: &[String]) -> Result<[u8; 32], KitError> {
    let seed = words::decode(words).map_err(|e| KitError::Words(capitalise(&e.to_string())))?;
    let fingerprint = recovery_public_key(&seed).fingerprint();
    let committed = log
        .state()
        .next
        .iter()
        .filter(|c| c.role == Role::Recovery)
        .any(|c| c.key == fingerprint);
    if committed {
        return Ok(seed);
    }
    // An entry this key signed: the kit's key was used after this kit was
    // made, and its words no longer restore.
    let earlier = log
        .entries()
        .iter()
        .skip(1)
        .any(|e| e.signer().fingerprint() == fingerprint);
    Err(if earlier {
        KitError::Spent
    } else {
        KitError::NotThisKit
    })
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Whether the Capsule at `root` holds anything an app made: an approved
/// app, a session or a stored file. A Capsule that does not may have its
/// unused ID replaced by a restore.
pub fn in_use(root: &Path) -> bool {
    let db = root.join("registry.db");
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return false;
    };
    ["grants", "sessions", "blobs"].iter().any(|table| {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })
        .is_ok_and(|n| n > 0)
    })
}

/// When the kit was last exported and when the person said they saved it:
/// `identity/kit`. A restore clears both, since the old kit is spent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KitStatus {
    pub exported_at: Option<u64>,
    pub saved_at: Option<u64>,
}

fn status_file(root: &Path) -> PathBuf {
    root.join("identity").join("kit")
}

pub fn status(root: &Path) -> KitStatus {
    let text = std::fs::read_to_string(status_file(root)).unwrap_or_default();
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.trim().parse().ok())
    };
    KitStatus {
        exported_at: field("exported_at "),
        saved_at: field("saved_at "),
    }
}

pub fn set_status(root: &Path, status: KitStatus) -> std::io::Result<()> {
    let mut text = String::new();
    if let Some(at) = status.exported_at {
        text.push_str(&format!("exported_at {at}\n"));
    }
    if let Some(at) = status.saved_at {
        text.push_str(&format!("saved_at {at}\n"));
    }
    write_whole(&status_file(root), text.as_bytes())
}

/// A kit given to the shell: the ID, the words, and the document to save.
#[derive(Debug, Clone)]
pub struct Exported {
    pub id: String,
    pub words: Vec<String>,
    pub document: String,
}

/// What a restore came to: the same ID, and the new kit that replaces the
/// spent one; `kit_made` says when the kit that was used had been made.
#[derive(Debug, Clone)]
pub struct Restored {
    pub id: String,
    pub words: Vec<String>,
    pub document: String,
    pub kit_made: u64,
}

/// Why export or restore was refused.
#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error("The person must ask for their recovery kit before it is shown.")]
    NotAsked,
    #[error("There is no Isonomic ID on this Mac yet.")]
    NoId,
    #[error(transparent)]
    Kit(#[from] KitError),
    #[error(transparent)]
    Identity(#[from] crate::IdentityError),
    #[error("Isonomic couldn't keep the recovery kit's record ({0}).")]
    Save(String),
}

impl crate::IdentityService {
    /// The person's recovery kit, for the shell, once the person asked.
    pub fn kit_export(&self, person_asked: bool) -> Result<Exported, RecoveryError> {
        if !person_asked {
            return Err(RecoveryError::NotAsked);
        }
        let person = &self.0.person;
        let Some(log) = person.read()? else {
            return Err(RecoveryError::NoId);
        };
        let seed = person.recovery_seed()?;
        let now = person.now();
        let words = words::encode(&seed);
        let kit = Kit {
            id: log.id().to_string(),
            made: now,
            words: words.clone(),
            log: log.entries().to_vec(),
        };
        let mut kept = status(person.root());
        kept.exported_at = Some(now);
        set_status(person.root(), kept).map_err(|e| RecoveryError::Save(e.to_string()))?;
        capsule_core::log::line("identity: recovery kit exported");
        Ok(Exported {
            id: kit.id.clone(),
            words,
            document: kit.to_document(),
        })
    }

    /// The person's ID from a recovery kit, on this Mac. `replace_unused`
    /// lets a Capsule whose own ID holds no app data have that ID set
    /// aside (never deleted) for the kit's.
    pub fn kit_restore(
        &self,
        document: &str,
        words: &[String],
        replace_unused: bool,
    ) -> Result<Restored, RecoveryError> {
        let person = &self.0.person;
        let kit = Kit::from_document(document)?;
        let log = kit.open()?;
        let seed = seed_for(&log, words)?;
        if let Some(existing) = person.peek()? {
            if existing.to_string() == kit.id || !replace_unused {
                return Err(KitError::HasId.into());
            }
            if in_use(person.root()) {
                return Err(KitError::InUse.into());
            }
            let stamp = registry::check::stamp(person.now());
            let to = person
                .set_aside(&stamp)
                .map_err(|e| RecoveryError::Save(e.to_string()))?;
            capsule_core::log::line(&format!(
                "identity: the unused ID {existing} was set aside in {} for a recovery kit's",
                to.display()
            ));
        }
        let (restored, fresh) = person.restore(log, &seed)?;
        self.0.forget();
        let now = person.now();
        let words = words::encode(&fresh);
        let new = Kit {
            id: restored.id().to_string(),
            made: now,
            words: words.clone(),
            log: restored.entries().to_vec(),
        };
        let reset = KitStatus {
            exported_at: Some(now),
            saved_at: None,
        };
        set_status(person.root(), reset).map_err(|e| RecoveryError::Save(e.to_string()))?;
        capsule_core::log::line("identity: restored from a recovery kit; that kit is now spent");
        Ok(Restored {
            id: new.id.clone(),
            words,
            document: new.to_document(),
            kit_made: kit.made,
        })
    }

    /// The person said they saved the kit last exported.
    pub fn kit_saved(&self) -> Result<(), RecoveryError> {
        let person = &self.0.person;
        let mut kept = status(person.root());
        kept.saved_at = Some(person.now());
        set_status(person.root(), kept).map_err(|e| RecoveryError::Save(e.to_string()))
    }

    /// When the kit was last exported and saved.
    pub fn kit_status(&self) -> KitStatus {
        status(self.0.person.root())
    }
}
