//! Apps get an Isonomic ID and a grant (R7, step 3).
//!
//! On `apps.grant`, an app gets:
//!
//! - its own key log, kind app, naming the person as its controller, with
//!   one Ed25519 key in the key store (`capsule/<id>/app/<app>/signing`),
//!   accepted into the directory;
//! - its space and the resources its manifest declares: each schema, and
//!   its events;
//! - grants from the person, each a one-link chain that cannot be passed on:
//!   read and write on its space, and read on each other app's schema its
//!   manifest lists. No grant expires; one ends only by revocation.
//!
//! A re-grant with the same capabilities keeps the grants. A changed one
//! revokes the old grants, kept first in `identity/revoked`, then issues the
//! new ones to `identity/grants/<app>.grants`. The app's token stays as it
//! was issued: the engine finds the grants by the app the token stands for.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use id_core::log::{Kind, Log, Op};
use id_core::{KeyAlg, PrincipalId, Signer};
use id_grants::{Action, Grant, GrantId, SignedGrant};
use id_resource::{ResourceId, Selector};
use isonomic_proto::Manifest;

use crate::files::{hex, unhex, write_whole};
use crate::person::{key, IdentityError, Person};
use crate::resources::{kind, Resources};

fn action(name: &str) -> Action {
    Action::new(name).expect("a valid action name")
}

/// The actions version 1 checks.
pub fn read() -> Action {
    action("read")
}
pub fn write() -> Action {
    action("write")
}

/// One grant an app should hold: what it covers and what it allows.
type Wanted = (Selector, BTreeSet<Action>);

pub struct AppIds {
    root: PathBuf,
    /// One change at a time: grants are replaced whole.
    lock: Mutex<()>,
}

/// A file-safe name for an app id (reverse-DNS ids are already safe; this
/// keeps it so for any id the manifest rules allow).
fn file_part(app: &str) -> String {
    app.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl AppIds {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.join("identity"),
            lock: Mutex::new(()),
        }
    }

    fn grants_file(&self, app: &str) -> PathBuf {
        self.root
            .join("grants")
            .join(format!("{}.grants", file_part(app)))
    }

    fn principal_file(&self, app: &str) -> PathBuf {
        self.root
            .join("apps")
            .join(format!("{}.id", file_part(app)))
    }

    /// The app's Isonomic ID, if it has one.
    pub fn principal(&self, app: &str) -> Option<PrincipalId> {
        let text = std::fs::read_to_string(self.principal_file(app)).ok()?;
        PrincipalId::try_from(text.trim().to_string()).ok()
    }

    /// The grants the app holds now.
    pub fn grants(&self, app: &str) -> Vec<SignedGrant> {
        let Ok(text) = std::fs::read_to_string(self.grants_file(app)) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| unhex(l.trim()))
            .filter_map(|b| SignedGrant::from_bytes(&b).ok())
            .collect()
    }

    /// Every grant ever revoked.
    pub fn revoked(&self) -> BTreeSet<GrantId> {
        let text = std::fs::read_to_string(self.root.join("revoked")).unwrap_or_default();
        text.lines()
            .filter_map(|l| unhex(l.trim()))
            .filter_map(|b| <[u8; 32]>::try_from(b).ok())
            .map(id_core::Digest::from_bytes)
            .collect()
    }

    /// Give `manifest`'s app its ID, resources and grants, as R7 says.
    pub fn granted(
        &self,
        person: &Person,
        resources: &Resources,
        manifest: &Manifest,
    ) -> Result<(), IdentityError> {
        let _one = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let app = manifest.id.as_str();
        // The person's ID is read from the Capsule; the key that signs for
        // it is opened only when there is something to sign. Approving an
        // app again with the same capabilities (every launch does) then
        // never touches the key store, so a scratch Capsule's second start,
        // whose in-memory keys are gone, approves Chat as the first did.
        let (owner, _) = person.get()?;
        let recipient = self.ensure_principal(person, &owner, app)?;
        let now_ms = person.now() * 1000;
        let saved = |e: rusqlite::Error| IdentityError::Save(e.to_string());

        let space = resources.space(app, now_ms).map_err(saved)?;
        for schema in &manifest.schemas {
            resources
                .within(app, kind::SCHEMA, &schema.name, now_ms)
                .map_err(saved)?;
        }
        resources
            .within(app, kind::EVENTS, kind::EVENTS, now_ms)
            .map_err(saved)?;
        let mut wanted: Vec<Wanted> = vec![(Selector::Collection(space), [read(), write()].into())];
        for listed in manifest.capabilities.sql.iter().flat_map(|s| &s.schemas) {
            let schema = resources.schema(listed, now_ms).map_err(saved)?;
            wanted.push((Selector::One(schema), [read()].into()));
        }

        let held = self.grants(app);
        let same = held.len() == wanted.len()
            && wanted.iter().all(|(sel, act)| {
                held.iter().any(|g| {
                    g.grant().recipient == recipient
                        && g.grant().resource == *sel
                        && g.grant().actions == *act
                })
            });
        if same {
            return Ok(());
        }
        let (_, signer) = person.signer()?;
        let issued = wanted
            .into_iter()
            .map(|(selector, actions)| {
                Grant::from_owner(owner.clone(), recipient.clone(), selector, actions).sign(&signer)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| IdentityError::KeyStore(format!("{e:?}")))?;
        // Revoke the old grants before the new ones are kept, so a crash
        // between the two leaves the app with none rather than both.
        if !held.is_empty() {
            let mut revoked =
                std::fs::read_to_string(self.root.join("revoked")).unwrap_or_default();
            for grant in &held {
                revoked.push_str(&grant.id().to_hex());
                revoked.push('\n');
            }
            write_whole(&self.root.join("revoked"), revoked.as_bytes())
                .map_err(|e| IdentityError::Save(e.to_string()))?;
        }
        let mut text = String::new();
        for grant in &issued {
            let bytes = grant
                .to_bytes()
                .map_err(|e| IdentityError::Save(format!("{e:?}")))?;
            text.push_str(&hex(&bytes));
            text.push('\n');
        }
        write_whole(&self.grants_file(app), text.as_bytes())
            .map_err(|e| IdentityError::Save(e.to_string()))
    }

    /// The app's ID, made on its first grant.
    fn ensure_principal(
        &self,
        person: &Person,
        owner: &PrincipalId,
        app: &str,
    ) -> Result<PrincipalId, IdentityError> {
        if let Some(id) = self.principal(app) {
            return Ok(id);
        }
        let backend = person.backend()?;
        let label = format!(
            "capsule/{}/app/{}/signing",
            person.capsule_id(),
            file_part(app)
        );
        let signing = key(&backend.keys, &label, KeyAlg::Ed25519)?;
        let log = Log::create(
            Kind::App,
            Some(owner.clone()),
            vec![Op::add(signing.public_key())],
            Vec::new(),
            person.now(),
            &signing,
        )
        .map_err(|e| IdentityError::KeyStore(format!("{e:?}")))?;
        // The directory keeps the log; the ID file says which app it is.
        let _ = person.logs().accept(log.entries());
        write_whole(
            &self.principal_file(app),
            format!("{}\n", log.id()).as_bytes(),
        )
        .map_err(|e| IdentityError::Save(e.to_string()))?;
        Ok(log.id().clone())
    }
}

/// Grant ids in hex, for tests and logs.
pub fn grant_ids(grants: &[SignedGrant]) -> Vec<String> {
    grants.iter().map(|g| g.id().to_hex()).collect()
}

/// The resource a grant covers, for tests.
pub fn covers(grant: &SignedGrant) -> Option<ResourceId> {
    match grant.grant().resource {
        Selector::Collection(id) | Selector::One(id) | Selector::Tree(id) => Some(id),
        Selector::Any => None,
    }
}
