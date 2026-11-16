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
use id_core::{KeyAlg, PrincipalId, Signer};
use id_keys::{KeyError, KeyHandle, KeyStore, StoredKey};

use crate::directory::Logs;
use crate::recovery::{new_seed, recovery_public_key, SEED_BYTES};
use crate::secrets::Secrets;

/// The key store and secrets a person's ID is made with.
#[derive(Clone)]
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
    root: PathBuf,
    file: PathBuf,
    capsule_id: String,
    open: OpenBackend,
    clock: Arc<dyn Clock>,
    /// Every log the engine has accepted, the person's own included.
    logs: Arc<Logs>,
    /// The ID once known; the lock also keeps two first calls from both
    /// making one.
    known: Mutex<Option<PrincipalId>>,
    /// The key store, once opened: only to make or sign, never to show.
    backend: Mutex<Option<Backend>>,
}

impl Person {
    /// The person of the Capsule whose root is `root` and whose marker names
    /// `capsule_id`.
    pub fn new(root: &Path, capsule_id: &str, open: OpenBackend, clock: Arc<dyn Clock>) -> Self {
        Self {
            root: root.to_path_buf(),
            file: root.join("identity").join("person.log"),
            capsule_id: capsule_id.to_string(),
            open,
            clock,
            logs: Arc::new(Logs::open(root)),
            known: Mutex::new(None),
            backend: Mutex::new(None),
        }
    }

    /// The key store, opened the first time it is needed.
    pub(crate) fn backend(&self) -> Result<Backend, IdentityError> {
        let mut kept = self.backend.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(backend) = kept.as_ref() {
            return Ok(backend.clone());
        }
        let backend = (self.open)().map_err(IdentityError::Unavailable)?;
        *kept = Some(backend.clone());
        Ok(backend)
    }

    /// The person's ID and the key that signs for it, making the ID first
    /// if there is none. Signing is what needs the key store after an ID
    /// exists (an unsigned build's keychain may ask the person to allow it).
    pub fn signer(&self) -> Result<(PrincipalId, StoredKey), IdentityError> {
        let (id, _) = self.get()?;
        let backend = self.backend()?;
        let handle = KeyHandle(self.label("device-signing"));
        let key = StoredKey::open(backend.keys, handle)
            .map_err(|e| IdentityError::KeyStore(e.to_string()))?;
        Ok((id, key))
    }

    /// When now is, for what the person signs.
    pub(crate) fn now(&self) -> u64 {
        self.clock.now_unix().max(0) as u64
    }

    /// The Capsule's id, which labels its key store items.
    pub(crate) fn capsule_id(&self) -> &str {
        &self.capsule_id
    }

    /// The Capsule's root folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The person's ID if it has been made, without making it.
    pub fn peek(&self) -> Result<Option<PrincipalId>, IdentityError> {
        let mut known = self.known.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(id) = known.as_ref() {
            return Ok(Some(id.clone()));
        }
        let found = self.read()?.map(|log| log.id().clone());
        if let Some(id) = &found {
            *known = Some(id.clone());
        }
        Ok(found)
    }

    /// The Capsule's directory of accepted logs.
    pub fn logs(&self) -> &Arc<Logs> {
        &self.logs
    }

    /// The person's ID, and whether this call made it.
    pub fn get(&self) -> Result<(PrincipalId, bool), IdentityError> {
        let mut known = self.known.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(id) = known.as_ref() {
            return Ok((id.clone(), false));
        }
        let (log, created) = match self.read()? {
            Some(log) => (log, false),
            None => {
                let log = self.make()?;
                self.write(&log)?;
                (log, true)
            }
        };
        // The person's own log is in the directory like any other. A copy
        // that parts ways with the one accepted before is kept as a fork
        // for the person to decide (`Logs::forks`); the ID itself stands.
        let _ = self.logs.accept(log.entries());
        *known = Some(log.id().clone());
        Ok((log.id().clone(), created))
    }

    fn label(&self, what: &str) -> String {
        format!("capsule/{}/person/{what}", self.capsule_id)
    }

    fn make(&self) -> Result<Log, IdentityError> {
        let backend = self.backend()?;
        let seed = self.seed(backend.secrets.as_ref())?;
        let signing = key(&backend.keys, &self.label("device-signing"), KeyAlg::P256)?;
        let rotation = key(&backend.keys, &self.label("device-rotation"), KeyAlg::P256)?;
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
        let unreadable = |e: crate::files::FileError| IdentityError::Unreadable(e.to_string());
        let Some(entries) = crate::files::read_log(&self.file).map_err(unreadable)? else {
            return Ok(None);
        };
        Log::open(entries, &crate::directory::checks())
            .map(Some)
            .map_err(|e| IdentityError::Unreadable(format!("{e:?}")))
    }

    fn write(&self, log: &Log) -> Result<(), IdentityError> {
        crate::files::write_log(&self.file, log.entries())
            .map_err(|e| IdentityError::Save(e.to_string()))
    }
}

/// The key under `label`, made with `alg` if it is not kept yet.
pub(crate) fn key(
    store: &Arc<dyn KeyStore>,
    label: &str,
    alg: KeyAlg,
) -> Result<StoredKey, IdentityError> {
    let failed = |e: KeyError| IdentityError::KeyStore(e.to_string());
    match StoredKey::open(Arc::clone(store), KeyHandle(label.to_string())) {
        Ok(key) => Ok(key),
        Err(KeyError::NotFound) => {
            StoredKey::generate(Arc::clone(store), alg, label).map_err(failed)
        }
        Err(e) => Err(failed(e)),
    }
}
