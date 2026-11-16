//! The person's Isonomic ID: made the first time it is asked for, then
//! read from the Capsule.
//!
//! Making it takes three things from the computer's key store, each labelled
//! for the Capsule (`capsule/<capsule id>/person/…`):
//!
//! - a P-256 signing key, in the secure chip when the build may use it,
//!   else in the login keychain (an unsigned build always gets the keychain);
//! - a P-256 device rotation key, for the log's next entry;
//! - the recovery kit's 32-byte seed, from which the recovery key is
//!   derived and committed (`recovery`).
//!
//! The key log is then written to `identity/person.log` in the Capsule: one
//! entry per line, its signed envelope in hexadecimal, exactly the bytes
//! the identity library made. Each step can be repeated after a crash
//! part-way: a key or seed already kept is used again, and the log file is
//! written whole and renamed into place, so it never holds half an ID.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use capsule_core::Clock;
use id_core::log::{Commitment, Kind, Log, Op};
use id_core::{KeyAlg, PrincipalId, Signed, Signer};
use id_keys::{KeyError, KeyHandle, KeyStore, StoredKey};

use crate::recovery::{new_seed, recovery_public_key, SEED_BYTES};
use crate::secrets::Secrets;

/// The key store and secrets a person's ID is made with.
pub struct Backend {
    pub keys: Arc<dyn KeyStore>,
    pub secrets: Arc<dyn Secrets>,
}

/// Opens the backend the first time an ID is made, not at engine start, so
/// a Capsule whose ID exists never touches the key store just to show it.
pub type OpenBackend = Box<dyn Fn() -> Result<Backend, String> + Send + Sync>;

/// Why there is no ID, in words the window shows as written.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdentityError {
    #[error("{0}")]
    Unavailable(String),
    #[error(
        "Your computer's key store didn't let Isonomic make your ID ({0}). \
         If it's locked, unlock it, then try again."
    )]
    KeyStore(String),
    #[error(
        "Isonomic couldn't save your ID in its data folder ({0}). Nothing was made; try again."
    )]
    Save(String),
    #[error("Your Isonomic ID's record can't be read ({0}). Nothing was changed.")]
    Unreadable(String),
}

pub struct Person {
    file: PathBuf,
    capsule_id: String,
    open: OpenBackend,
    clock: Arc<dyn Clock>,
    /// The ID once known; the lock also keeps two first calls from both
    /// making one.
    known: Mutex<Option<PrincipalId>>,
}

impl Person {
    /// The person of the Capsule whose root is `root` and whose marker names
    /// `capsule_id`.
    pub fn new(root: &Path, capsule_id: &str, open: OpenBackend, clock: Arc<dyn Clock>) -> Self {
        Self {
            file: root.join("identity").join("person.log"),
            capsule_id: capsule_id.to_string(),
            open,
            clock,
            known: Mutex::new(None),
        }
    }

    /// The person's ID, and whether this call made it.
    pub fn get(&self) -> Result<(PrincipalId, bool), IdentityError> {
        let mut known = self.known.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(id) = known.as_ref() {
            return Ok((id.clone(), false));
        }
        if let Some(log) = self.read()? {
            *known = Some(log.id().clone());
            return Ok((log.id().clone(), false));
        }
        let log = self.make()?;
        self.write(&log)?;
        *known = Some(log.id().clone());
        Ok((log.id().clone(), true))
    }

    fn label(&self, what: &str) -> String {
        format!("capsule/{}/person/{what}", self.capsule_id)
    }

    fn make(&self) -> Result<Log, IdentityError> {
        let backend = (self.open)().map_err(IdentityError::Unavailable)?;
        let seed = self.seed(backend.secrets.as_ref())?;
        let signing = key(&backend.keys, &self.label("device-signing"))?;
        let rotation = key(&backend.keys, &self.label("device-rotation"))?;
        let next = vec![
            Commitment::device(&rotation.public_key()),
            Commitment::recovery(&recovery_public_key(&seed)),
        ];
        let at = self.clock.now_unix().max(0) as u64;
        let ops = vec![Op::add(signing.public_key())];
        // An Ethereum binding at creation would be one `Log::append` here,
        // signed by `rotation`, committing a fresh device rotation key: a
        // log may not bind in its first entry.
        Log::create(Kind::Person, None, ops, next, at, &signing)
            .map_err(|e| IdentityError::KeyStore(format!("{e:?}")))
    }

    /// The recovery seed: kept before anything commits to it.
    fn seed(&self, secrets: &dyn Secrets) -> Result<[u8; SEED_BYTES], IdentityError> {
        let name = self.label("recovery-seed");
        if let Some(kept) = secrets.get(&name).map_err(IdentityError::KeyStore)? {
            return kept.try_into().map_err(|_| {
                IdentityError::KeyStore("the kept recovery seed is not 32 bytes".into())
            });
        }
        let seed = new_seed().map_err(|_| IdentityError::KeyStore("no random bytes".into()))?;
        secrets.put(&name, &seed).map_err(IdentityError::KeyStore)?;
        Ok(seed)
    }

    fn read(&self) -> Result<Option<Log>, IdentityError> {
        let text = match std::fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(IdentityError::Unreadable(e.to_string())),
        };
        let entries = text
            .lines()
            .map(|line| unhex(line.trim()).and_then(|b| Signed::from_bytes(&b).ok()))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| IdentityError::Unreadable("an entry does not decode".into()))?;
        Log::open(entries, &[])
            .map(Some)
            .map_err(|e| IdentityError::Unreadable(format!("{e:?}")))
    }

    fn write(&self, log: &Log) -> Result<(), IdentityError> {
        let save = |e: std::io::Error| IdentityError::Save(e.to_string());
        let mut text = String::new();
        for entry in log.entries() {
            let bytes = entry
                .to_bytes()
                .map_err(|e| IdentityError::Save(format!("{e:?}")))?;
            text.push_str(&hex(&bytes));
            text.push('\n');
        }
        let dir = self.file.parent().expect("identity/");
        std::fs::create_dir_all(dir).map_err(save)?;
        let part = self.file.with_extension("log.part");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&part).map_err(save)?;
            file.write_all(text.as_bytes()).map_err(save)?;
            file.sync_all().map_err(save)?;
        }
        std::fs::rename(&part, &self.file).map_err(save)
    }
}

/// The key under `label`, made if it is not kept yet.
fn key(store: &Arc<dyn KeyStore>, label: &str) -> Result<StoredKey, IdentityError> {
    let failed = |e: KeyError| IdentityError::KeyStore(e.to_string());
    match StoredKey::open(Arc::clone(store), KeyHandle(label.to_string())) {
        Ok(key) => Ok(key),
        Err(KeyError::NotFound) => {
            StoredKey::generate(Arc::clone(store), KeyAlg::P256, label).map_err(failed)
        }
        Err(e) => Err(failed(e)),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}
