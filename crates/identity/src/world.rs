//! What the check function sees (R7, step 4): keys from the directory of
//! logs, revocations from `identity/revoked`, resources from `registry.db`.
//! Its version moves whenever the directory or the revocations change, so
//! cached chains (`ChainCache`) are dropped exactly then.

use std::collections::BTreeSet;

use id_check::{VersionedWorld, World};
use id_core::{KeyResolver, PrincipalId, PublicKey, Timestamp};
use id_grants::{GrantId, Revocations};
use id_resource::{ResourceId, ResourceLookup, ResourceMeta};

use crate::directory::Logs;
use crate::resources::Resources;

pub struct CapsuleWorld<'a> {
    pub logs: &'a Logs,
    pub resources: &'a Resources,
    pub revoked: &'a BTreeSet<GrantId>,
}

impl KeyResolver for CapsuleWorld<'_> {
    fn is_current_key(&self, who: &PrincipalId, key: &PublicKey, at: Timestamp) -> bool {
        self.logs.is_current_key(who, key, at)
    }
}

impl Revocations for CapsuleWorld<'_> {
    fn is_revoked(&self, id: &GrantId) -> bool {
        self.revoked.contains(id)
    }
}

impl ResourceLookup for CapsuleWorld<'_> {
    fn resource(&self, id: &ResourceId) -> Option<ResourceMeta> {
        self.resources.resource(id)
    }
}

impl World for CapsuleWorld<'_> {}

impl VersionedWorld for CapsuleWorld<'_> {
    fn version(&self) -> u64 {
        // Both only grow while the engine runs: the directory's version on
        // every accepted change, the revocations as they are added.
        (self.logs.version() << 32) | self.revoked.len() as u64
    }
}

/// The decision for `principal` doing `action` on `resource`, given the
/// grants it holds. Each grant an app holds is its own one-link chain, so
/// each is tried alone, through `cache` so a warm call is one lookup; the
/// first that allows decides; else asking a person, if a grant says so;
/// else the last denial (deny by default).
pub fn decide(
    world: &CapsuleWorld<'_>,
    cache: &id_check::ChainCache,
    principal: &PrincipalId,
    action: &id_grants::Action,
    resource: &ResourceId,
    grants: &[id_grants::SignedGrant],
    now: Timestamp,
) -> id_check::Decision {
    let mut last = id_check::Decision::Deny(id_check::Reason::NoProof);
    let mut ask = None;
    for grant in grants {
        let proof = std::slice::from_ref(grant);
        let request = id_check::Request {
            principal,
            action,
            resource,
            proof,
            context: id_check::Context::at(now),
        };
        let decision = cache.check(&request, world);
        match decision {
            id_check::Decision::Allow(_) => return decision,
            // Asking a person beats a denial from another grant.
            id_check::Decision::Ask(_) => ask = Some(decision),
            id_check::Decision::Deny(_) => last = decision,
        }
    }
    ask.unwrap_or(last)
}
