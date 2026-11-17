// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Every key log the engine has accepted (R7, step 2): the person's own,
//! and each app's and agent's, kept across restarts.
//!
//! The identity library's `Directory` verifies a log and remembers it: a
//! longer continuation replaces what it knew, an older copy changes nothing,
//! and a log that parts ways with the known one is refused as a fork. This
//! keeps what it accepts in the Capsule, one file per identity under
//! `identity/logs/`, written before the directory in memory changes
//! (persist, then publish), and takes them up again at start.
//!
//! A fork is never resolved here. The competing log is kept under
//! `identity/forks/`, with where it parts ways and whether a recovery key
//! signed it, for the person to decide; the known log stays in force.
//! A stored log that no longer verifies is moved to `identity/unreadable/`,
//! never deleted, and reported.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use id_core::log::{BindingCheck, Directory, LogError, State};
use id_core::{KeyResolver, PrincipalId, PublicKey, Signed, Timestamp};

use crate::files::{read_log, write_log};

/// The binding checkers every log is verified with: a log with a binding
/// verifies only when its scheme's checker is present.
pub fn checks() -> [&'static dyn BindingCheck; 1] {
    [&id_eth::EthBinding]
}

/// A log that parts ways with the one accepted for its identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fork {
    pub id: PrincipalId,
    /// The first entry where the two disagree, from zero.
    pub at: u64,
    /// The competing entry was signed by a recovery key.
    pub by_recovery: bool,
    /// Where the competing log is kept.
    pub file: PathBuf,
}

/// What became of a log shown to the directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Accepted {
    /// New, or a longer continuation: kept.
    Newer,
    /// What was already known, or an older copy of it.
    Known,
}

/// Why a log was not accepted.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// It does not verify.
    Invalid(LogError),
    /// It parts ways with the accepted log; kept for the person to decide.
    Fork(Fork),
    /// It could not be kept.
    Save(String),
}

struct Inner {
    directory: Directory,
    forks: Vec<Fork>,
}

pub struct Logs {
    root: PathBuf,
    inner: Mutex<Inner>,
    /// What loading found, for the engine's log.
    notes: Vec<String>,
}

/// The file an identity's log is kept in: the digest part of a real ID,
/// else the ID's text in hexadecimal.
fn file_name(id: &PrincipalId) -> String {
    let text = id.as_str();
    match text.strip_prefix("did:iso:") {
        Some(digest) if digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()) => {
            format!("{digest}.log")
        }
        _ => format!("{}.log", crate::files::hex(text.as_bytes())),
    }
}

impl Logs {
    /// The logs kept in the Capsule at `root`, verified again.
    pub fn open(root: &Path) -> Self {
        let root = root.join("identity");
        let mut directory = Directory::default();
        let mut notes = Vec::new();
        let dir = root.join("logs");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for path in files
            .into_iter()
            .filter(|p| p.extension() == Some("log".as_ref()))
        {
            let accepted = match read_log(&path) {
                Ok(Some(entries)) => directory
                    .accept(&entries, &checks())
                    .map(|_| ())
                    .map_err(|e| format!("{e:?}")),
                Ok(None) => continue,
                Err(e) => Err(e.to_string()),
            };
            if let Err(why) = accepted {
                let name = path.file_name().unwrap_or_default().to_owned();
                let aside = root.join("unreadable").join(&name);
                let moved = std::fs::create_dir_all(aside.parent().unwrap())
                    .and_then(|()| std::fs::rename(&path, &aside));
                notes.push(match moved {
                    Ok(()) => format!("identity log {name:?} does not verify ({why}); moved to identity/unreadable"),
                    Err(e) => format!("identity log {name:?} does not verify ({why}); left in place ({e})"),
                });
            }
        }
        Self {
            root,
            inner: Mutex::new(Inner {
                directory,
                forks: Vec::new(),
            }),
            notes,
        }
    }

    /// What loading found that a person should know: logs set aside.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Verify `log`, keep it if it is new or longer, and remember it.
    pub fn accept(&self, log: &[Signed]) -> Result<Accepted, Refused> {
        let mut inner = self.lock();
        let mut next = inner.directory.clone();
        let accepted = next.accept(log, &checks()).map(|state| state.id.clone());
        match accepted {
            Ok(id) => {
                if next.version() == inner.directory.version() {
                    return Ok(Accepted::Known);
                }
                let file = self.root.join("logs").join(file_name(&id));
                write_log(&file, log).map_err(|e| Refused::Save(e.to_string()))?;
                inner.directory = next;
                Ok(Accepted::Newer)
            }
            Err(LogError::Fork { at, by_recovery }) => {
                let id = id_core::log::replay(log, &checks())
                    .map(|s| s.id)
                    .map_err(Refused::Invalid)?;
                let n = inner.forks.len();
                let name = format!(
                    "{}-at-{at}-{n}.log",
                    file_name(&id).trim_end_matches(".log")
                );
                let file = self.root.join("forks").join(name);
                write_log(&file, log).map_err(|e| Refused::Save(e.to_string()))?;
                let fork = Fork {
                    id,
                    at,
                    by_recovery,
                    file,
                };
                inner.forks.push(fork.clone());
                Err(Refused::Fork(fork))
            }
            Err(e) => Err(Refused::Invalid(e)),
        }
    }

    /// The forks refused since start, for the person to decide.
    pub fn forks(&self) -> Vec<Fork> {
        self.lock().forks.clone()
    }

    /// Goes up whenever an accepted log changes what is known; caches of
    /// verified grants compare it (`VersionedWorld`).
    pub fn version(&self) -> u64 {
        self.lock().directory.version()
    }

    /// The last accepted state of an identity.
    pub fn state(&self, id: &PrincipalId) -> Option<State> {
        self.lock().directory.state(id).cloned()
    }
}

impl KeyResolver for Logs {
    fn is_current_key(&self, who: &PrincipalId, key: &PublicKey, at: Timestamp) -> bool {
        self.lock().directory.is_current_key(who, key, at)
    }
}
