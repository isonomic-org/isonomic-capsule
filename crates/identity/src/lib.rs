//! The person's Isonomic ID (isonomic-sdk request R7, step 1), served to the
//! window as `identity.get`.
//!
//! The ID is made the first time it is asked for, from the Mac's key store,
//! and kept in the Capsule (`person`). On a computer without a Mac key store
//! (Linux, in CI and development) it is unavailable, in plain words; tests
//! give it the identity library's in-memory store instead.

pub mod directory;
mod files;
mod person;
pub mod recovery;
mod secrets;

use std::sync::Arc;

use broker::agents::BoxFuture;
use isonomic_proto::identity::Identity;
use isonomic_proto::{ErrorCode, WireError};

pub use person::{Backend, IdentityError, OpenBackend, Person};
#[cfg(target_os = "macos")]
pub use secrets::MacSecrets;
pub use secrets::{MemorySecrets, Secrets};

/// What a person reads when this computer cannot make an ID.
pub const NO_KEY_STORE: &str =
    "Isonomic IDs are made with a Mac's key store, and this computer has none.";

/// The backend the engine uses: the Mac's key store and keychain, or none.
pub fn system_backend() -> OpenBackend {
    #[cfg(target_os = "macos")]
    {
        Box::new(|| {
            Ok(Backend {
                keys: Arc::new(id_keys::MacKeyStore::new()),
                secrets: Arc::new(MacSecrets),
            })
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(|| Err(NO_KEY_STORE.to_string()))
    }
}

/// The identity library's in-memory stores, for tests: an ID made with it
/// lasts only while the process does, though its log stays in the Capsule.
pub fn memory_backend() -> OpenBackend {
    let keys: Arc<dyn id_keys::KeyStore> = Arc::new(id_keys::MemoryKeyStore::new());
    let secrets: Arc<dyn Secrets> = Arc::new(MemorySecrets::default());
    Box::new(move || {
        Ok(Backend {
            keys: Arc::clone(&keys),
            secrets: Arc::clone(&secrets),
        })
    })
}

/// `identity.get` for the broker.
pub struct IdentityService(Arc<Person>);

impl IdentityService {
    pub fn new(person: Person) -> Self {
        Self(Arc::new(person))
    }
}

impl broker::Identity for IdentityService {
    fn get(&self) -> BoxFuture<'_, Result<Identity, WireError>> {
        let person = Arc::clone(&self.0);
        Box::pin(async move {
            // The key store may block on the system; keep it off the runtime.
            let made = tokio::task::spawn_blocking(move || person.get()).await;
            let unavailable = |m: String| WireError::new(ErrorCode::IdentityUnavailable, m);
            match made {
                Ok(Ok((id, created))) => Ok(Identity {
                    id: id.as_str().to_string(),
                    created,
                }),
                Ok(Err(e)) => Err(unavailable(e.to_string())),
                Err(e) => Err(unavailable(format!(
                    "Isonomic couldn't make your ID ({e}).",
                ))),
            }
        })
    }
}
