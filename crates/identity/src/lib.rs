//! The person's Isonomic ID (isonomic-sdk request R7, step 1), served to the
//! window as `identity.get`.
//!
//! The ID is made the first time it is asked for, from the computer's key store,
//! and kept in the Capsule (`person`). On a computer without a Mac key store
//! (Linux, in CI and development) it is unavailable, in plain words; tests
//! give it the identity library's in-memory store instead.

pub mod apps;
pub mod directory;
mod files;
mod person;
pub mod recovery;
pub mod resources;
mod secrets;
pub mod world;

use std::sync::{Arc, OnceLock};

use broker::agents::BoxFuture;
use isonomic_proto::identity::Identity;
use isonomic_proto::{ErrorCode, WireError};

pub use person::{Backend, IdentityError, OpenBackend, Person};
#[cfg(target_os = "macos")]
pub use secrets::MacSecrets;
pub use secrets::{MemorySecrets, Secrets};

/// What a person reads when this computer cannot make an ID.
pub const NO_KEY_STORE: &str =
    "Isonomic IDs are made with your computer's key store, and this computer has none Isonomic can use.";

/// The backend the engine uses: the computer's key store, or none.
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

/// The person's Isonomic ID, and apps' IDs and grants, for the broker.
#[derive(Clone)]
pub struct IdentityService(Arc<Inner>);

struct Inner {
    person: Person,
    apps: apps::AppIds,
    /// Opened once the person's ID exists: the person owns every resource.
    resources: OnceLock<resources::Resources>,
    /// Every approved app, for the grants that follow the person's ID.
    approved: Option<Arc<broker::Apps>>,
}

impl IdentityService {
    pub fn new(person: Person) -> Self {
        Self(Arc::new(Inner {
            apps: apps::AppIds::new(person.root()),
            person,
            resources: OnceLock::new(),
            approved: None,
        }))
    }

    /// Issue grants to the apps approved before the person's ID existed,
    /// or before apps got grants at all (`fill_in`).
    pub fn with_apps(self, approved: Arc<broker::Apps>) -> Self {
        let inner = Arc::try_unwrap(self.0).unwrap_or_else(|_| panic!("set up before use"));
        Self(Arc::new(Inner {
            approved: Some(approved),
            ..inner
        }))
    }

    /// Give every approved app without grants its ID and grants. Safe to
    /// repeat; does nothing while the person has no ID. Returns how many
    /// apps were given grants.
    pub fn fill_in(&self) -> Result<usize, IdentityError> {
        self.0.fill_in()
    }
}

impl Inner {
    fn resources(
        &self,
        owner: &id_core::PrincipalId,
    ) -> Result<&resources::Resources, IdentityError> {
        if let Some(r) = self.resources.get() {
            return Ok(r);
        }
        let db = self.person.root().join("registry.db");
        let opened = resources::Resources::open(&db, owner.clone())
            .map_err(|e| IdentityError::Save(e.to_string()))?;
        Ok(self.resources.get_or_init(|| opened))
    }

    fn granted(&self, manifest: &isonomic_proto::Manifest) -> Result<(), IdentityError> {
        // Before the person has an ID there is no one to grant from; the
        // grants follow when it is made.
        let Some(owner) = self.person.peek()? else {
            return Ok(());
        };
        let resources = self.resources(&owner)?;
        self.apps.granted(&self.person, resources, manifest)
    }

    fn fill_in(&self) -> Result<usize, IdentityError> {
        let Some(approved) = &self.approved else {
            return Ok(0);
        };
        if self.person.peek()?.is_none() {
            return Ok(0);
        }
        let mut given = 0;
        for manifest in approved.manifests() {
            if self.apps.grants(&manifest.id).is_empty() {
                self.granted(&manifest)?;
                given += 1;
            }
        }
        Ok(given)
    }
}

fn unavailable(e: impl std::fmt::Display) -> WireError {
    WireError::new(ErrorCode::IdentityUnavailable, e.to_string())
}

impl broker::Identity for IdentityService {
    fn get(&self) -> BoxFuture<'_, Result<Identity, WireError>> {
        let inner = Arc::clone(&self.0);
        Box::pin(async move {
            // The key store may block on the system; keep it off the runtime.
            let made = tokio::task::spawn_blocking(move || {
                let (id, created) = inner.person.get()?;
                if created {
                    // Apps approved before the ID existed get their grants.
                    inner.fill_in()?;
                }
                Ok::<_, IdentityError>((id, created))
            })
            .await;
            match made {
                Ok(Ok((id, created))) => Ok(Identity {
                    id: id.as_str().to_string(),
                    created,
                }),
                Ok(Err(e)) => Err(unavailable(e)),
                Err(e) => Err(unavailable(format!(
                    "Isonomic couldn't make your ID ({e})."
                ))),
            }
        })
    }

    fn granted<'a>(
        &'a self,
        manifest: &'a isonomic_proto::Manifest,
    ) -> BoxFuture<'a, Result<(), WireError>> {
        let inner = Arc::clone(&self.0);
        let manifest = manifest.clone();
        Box::pin(async move {
            let done = tokio::task::spawn_blocking(move || inner.granted(&manifest)).await;
            match done {
                Ok(Ok(())) => Ok(()),
                // No key store on this computer: no IDs, so no grants; the
                // manifest's own checks still hold.
                Ok(Err(IdentityError::Unavailable(_))) => Ok(()),
                Ok(Err(e)) => Err(unavailable(e)),
                Err(e) => Err(unavailable(format!(
                    "Isonomic couldn't grant the app ({e})."
                ))),
            }
        })
    }
}
