// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The person's Isonomic ID (isonomic-sdk request R7, step 1), served to the
//! window as `identity.get`.
//!
//! The ID is made the first time it is asked for, from the computer's key store,
//! and kept in the Capsule (`person`). On a computer without a Mac key store
//! (Linux, in CI and development) it is unavailable, in plain words; tests
//! give it the identity library's in-memory store instead.

pub mod apps;
pub mod check;
pub mod directory;
mod files;
mod person;
pub mod recovery;
pub mod resources;
mod secrets;
pub mod store_choice;
pub mod world;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

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
    /// Verified chains, so a warm check is a lookup.
    chains: id_check::ChainCache,
    /// Each app's ID and grants, as last read; dropped on every change.
    held: Mutex<HashMap<String, Arc<Held>>>,
    /// Every revoked grant, as last read; dropped on every change.
    revoked: Mutex<Option<Arc<BTreeSet<id_grants::GrantId>>>>,
}

struct Held {
    principal: id_core::PrincipalId,
    grants: Vec<id_grants::SignedGrant>,
}

/// What the fast path of a check found.
enum Checked {
    Allowed,
    Refused(WireError),
    /// The app has no grants, or a resource is missing: issue them first.
    NeedsGrants,
}

impl IdentityService {
    pub fn new(person: Person) -> Self {
        Self(Arc::new(Inner {
            apps: apps::AppIds::new(person.root()),
            person,
            resources: OnceLock::new(),
            approved: None,
            chains: id_check::ChainCache::new(),
            held: Mutex::new(HashMap::new()),
            revoked: Mutex::new(None),
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
        let done = self.apps.granted(&self.person, resources, manifest);
        // Whatever happened, the next check reads the grants again.
        self.held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&manifest.id);
        *self.revoked.lock().unwrap_or_else(|p| p.into_inner()) = None;
        done
    }

    fn held(&self, app: &str) -> Option<Arc<Held>> {
        let mut all = self.held.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(held) = all.get(app) {
            return Some(Arc::clone(held));
        }
        let grants = self.apps.grants(app);
        let principal = self.apps.principal(app)?;
        if grants.is_empty() {
            return None;
        }
        let held = Arc::new(Held { principal, grants });
        all.insert(app.to_string(), Arc::clone(&held));
        Some(held)
    }

    fn revoked(&self) -> Arc<BTreeSet<id_grants::GrantId>> {
        let mut kept = self.revoked.lock().unwrap_or_else(|p| p.into_inner());
        Arc::clone(kept.get_or_insert_with(|| Arc::new(self.apps.revoked())))
    }

    /// The check for one call, without issuing anything (see `Checked`).
    fn check(&self, manifest: &isonomic_proto::Manifest, call: &isonomic_proto::Call) -> Checked {
        let Some((action, target)) = check::target(manifest, call) else {
            return Checked::Allowed;
        };
        // No ID on this computer yet (or none possible): the manifest's own
        // checks are what holds, as before the person had one.
        let owner = match self.person.peek() {
            Ok(Some(owner)) => owner,
            Ok(None) | Err(IdentityError::Unavailable(_)) => return Checked::Allowed,
            Err(e) => return Checked::Refused(unavailable(e)),
        };
        let app = manifest.id.as_str();
        let Some(held) = self.held(app) else {
            return Checked::NeedsGrants;
        };
        let resources = match self.resources(&owner) {
            Ok(r) => r,
            Err(e) => return Checked::Refused(unavailable(e)),
        };
        let found = match &target {
            check::Target::Space => resources.find(app, resources::kind::SPACE, app),
            check::Target::Schema(name) => resources.find("", resources::kind::SCHEMA, name),
            check::Target::Events => {
                resources.find(app, resources::kind::EVENTS, resources::kind::EVENTS)
            }
        };
        let Some(resource) = found else {
            return Checked::NeedsGrants;
        };
        let revoked = self.revoked();
        let world = world::CapsuleWorld {
            logs: self.person.logs(),
            resources,
            revoked: &revoked,
        };
        let now = self.person.now();
        let decision = world::decide(
            &world,
            &self.chains,
            &held.principal,
            &action,
            &resource,
            &held.grants,
            now,
        );
        let verb = action.as_str();
        match decision {
            id_check::Decision::Allow(_) => Checked::Allowed,
            id_check::Decision::Ask(_) => Checked::Refused(WireError::new(
                ErrorCode::CapabilityDenied,
                format!("{app} needs the person's approval before it can {verb} this."),
            )),
            id_check::Decision::Deny(reason) => Checked::Refused(WireError::new(
                ErrorCode::CapabilityDenied,
                format!("The person's grants don't let {app} {verb} this ({reason:?})."),
            )),
        }
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

    fn check<'a>(
        &'a self,
        manifest: &'a isonomic_proto::Manifest,
        call: &'a isonomic_proto::Call,
    ) -> BoxFuture<'a, Result<(), WireError>> {
        Box::pin(async move {
            match self.0.check(manifest, call) {
                Checked::Allowed => Ok(()),
                Checked::Refused(e) => Err(e),
                Checked::NeedsGrants => {
                    // An app approved before it had grants (a Capsule from an
                    // earlier build): issue them, then check again. The key
                    // store may block, so off the runtime.
                    let inner = Arc::clone(&self.0);
                    let (manifest, call) = (manifest.clone(), call.clone());
                    let app = manifest.id.clone();
                    let done = tokio::task::spawn_blocking(move || {
                        match inner.granted(&manifest) {
                            Ok(()) | Err(IdentityError::Unavailable(_)) => {}
                            Err(e) => return Checked::Refused(unavailable(e)),
                        }
                        inner.check(&manifest, &call)
                    })
                    .await;
                    match done {
                        Ok(Checked::Allowed) => Ok(()),
                        Ok(Checked::Refused(e)) => Err(e),
                        Ok(Checked::NeedsGrants) => Err(WireError::new(
                            ErrorCode::CapabilityDenied,
                            format!("{app} has no grant from the person for this."),
                        )),
                        Err(e) => Err(unavailable(format!(
                            "Isonomic couldn't check the call ({e})."
                        ))),
                    }
                }
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
