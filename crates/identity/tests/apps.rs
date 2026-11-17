// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Apps get an ID and grants from the person on `apps.grant` (R7 step 3),
//! checked with the identity library's own check function. Hermetic: the
//! library's in-memory key store stands in for the Mac's.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use capsule_core::ManualClock;
use id_check::{ChainCache, Decision, Reason};
use id_core::log::Kind;
use id_core::PrincipalId;
use id_grants::{Action, SignedGrant};
use id_resource::ResourceId;
use identity::apps::{read, write, AppIds};
use identity::directory::Logs;
use identity::resources::{kind, Resources};
use identity::world::CapsuleWorld;
use identity::{memory_backend, Person};
use isonomic_proto::manifest::{Schema, SqlAccess};
use isonomic_proto::{Capabilities, Manifest};

const NOW: u64 = 1_800_000_000;

fn manifest(name: &str, schemas: &[&str], reads: &[&str]) -> Manifest {
    Manifest {
        id: format!("com.isonomic.{name}"),
        name: name.into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            sql: (!reads.is_empty()).then(|| SqlAccess {
                schemas: reads.iter().map(|s| s.to_string()).collect(),
            }),
            ..Default::default()
        },
        schemas: schemas
            .iter()
            .map(|s| Schema {
                name: s.to_string(),
            })
            .collect(),
    }
}

struct Capsule {
    _dir: tempfile::TempDir,
    person: Person,
    owner: PrincipalId,
    resources: Resources,
    apps: AppIds,
}

fn capsule() -> Capsule {
    let dir = tempfile::tempdir().unwrap();
    let person = Person::new(
        dir.path(),
        &"c".repeat(32),
        memory_backend(),
        Arc::new(ManualClock::at(NOW as i64)),
    );
    let (owner, _) = person.get().unwrap();
    let resources = Resources::open(&dir.path().join("registry.db"), owner.clone()).unwrap();
    let apps = AppIds::new(dir.path());
    Capsule {
        _dir: dir,
        person,
        owner,
        resources,
        apps,
    }
}

impl Capsule {
    fn root(&self) -> &Path {
        self._dir.path()
    }

    fn grant(&self, m: &Manifest) {
        self.apps.granted(&self.person, &self.resources, m).unwrap();
    }

    fn ask(&self, app: &str, action: &Action, resource: &ResourceId) -> Decision {
        self.ask_with(app, action, resource, &self.apps.grants(app))
    }

    fn ask_with(
        &self,
        app: &str,
        action: &Action,
        resource: &ResourceId,
        proof: &[SignedGrant],
    ) -> Decision {
        let revoked = self.apps.revoked();
        let world = CapsuleWorld {
            logs: self.person.logs(),
            resources: &self.resources,
            revoked: &revoked,
        };
        let principal = self.apps.principal(app).expect("the app has an ID");
        identity::world::decide(
            &world,
            &ChainCache::new(),
            &principal,
            action,
            resource,
            proof,
            NOW,
        )
    }
}

#[test]
fn an_app_gets_its_own_id_naming_the_person_and_keeps_it() {
    let c = capsule();
    let notes = manifest("notes", &["notes"], &[]);
    c.grant(&notes);
    let id = c.apps.principal(&notes.id).expect("an ID");
    let state = c.person.logs().state(&id).expect("in the directory");
    assert_eq!(state.kind, Kind::App);
    assert_eq!(state.controller.as_ref(), Some(&c.owner));
    c.grant(&notes);
    assert_eq!(
        c.apps.principal(&notes.id),
        Some(id.clone()),
        "the same ID again"
    );
    assert_eq!(
        AppIds::new(c.root()).principal(&notes.id),
        Some(id.clone()),
        "after a restart"
    );
    assert!(Logs::open(c.root()).state(&id).is_some(), "its log is kept");
}

#[test]
fn the_space_grant_covers_the_apps_own_data_and_nothing_of_anothers() {
    let c = capsule();
    let (notes, chat) = (
        manifest("notes", &["notes"], &[]),
        manifest("chat", &["chat"], &[]),
    );
    c.grant(&notes);
    c.grant(&chat);
    let grants = c.apps.grants(&notes.id);
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].grant().issuer, c.owner);
    assert_eq!(grants[0].grant().delegations, 0, "an app passes nothing on");
    assert_eq!(grants[0].grant().expires, None);

    let schema = c.resources.find(&notes.id, kind::SCHEMA, "notes").unwrap();
    let events = c
        .resources
        .find(&notes.id, kind::EVENTS, kind::EVENTS)
        .unwrap();
    for (action, resource) in [
        (read(), schema),
        (write(), schema),
        (read(), events),
        (write(), events),
    ] {
        assert!(
            c.ask(&notes.id, &action, &resource).is_allowed(),
            "{action:?}"
        );
    }
    let theirs = c.resources.find(&chat.id, kind::SCHEMA, "chat").unwrap();
    assert!(
        !c.ask(&notes.id, &read(), &theirs).is_allowed(),
        "another app's schema"
    );
}

#[test]
fn a_listed_schema_can_be_read_and_not_written() {
    let c = capsule();
    let (notes, reader) = (
        manifest("notes", &["notes"], &[]),
        manifest("reader", &["reader"], &["notes"]),
    );
    c.grant(&notes);
    c.grant(&reader);
    let theirs = c.resources.find(&notes.id, kind::SCHEMA, "notes").unwrap();
    assert!(c.ask(&reader.id, &read(), &theirs).is_allowed());
    assert_eq!(
        c.ask(&reader.id, &write(), &theirs),
        Decision::Deny(Reason::ActionNotGranted)
    );
}

#[test]
fn a_schema_listed_before_its_owner_is_granted_is_claimed_by_it() {
    let c = capsule();
    let (notes, reader) = (
        manifest("notes", &["notes"], &[]),
        manifest("reader", &["reader"], &["notes"]),
    );
    c.grant(&reader);
    let early = c
        .resources
        .find("", kind::SCHEMA, "notes")
        .expect("recorded on its own");
    assert!(c.ask(&reader.id, &read(), &early).is_allowed());
    c.grant(&notes);
    let claimed = c.resources.find(&notes.id, kind::SCHEMA, "notes").unwrap();
    assert_eq!(
        claimed, early,
        "the same resource, now in the owner's space"
    );
    assert!(c.ask(&notes.id, &write(), &claimed).is_allowed());
    assert!(c.ask(&reader.id, &read(), &claimed).is_allowed());
}

#[test]
fn the_same_grant_keeps_its_grants_and_a_changed_one_revokes_them() {
    let c = capsule();
    c.grant(&manifest("notes", &["notes"], &[]));
    let reader = manifest("reader", &["reader"], &[]);
    c.grant(&reader);
    let before = c.apps.grants(&reader.id);
    c.grant(&reader);
    assert_eq!(c.apps.grants(&reader.id), before, "nothing changed");
    assert!(c.apps.revoked().is_empty());

    c.grant(&manifest("reader", &["reader"], &["notes"]));
    let after = c.apps.grants(&reader.id);
    assert_eq!(after.len(), 2);
    let revoked = c.apps.revoked();
    let old: BTreeSet<_> = before.iter().map(|g| g.id()).collect();
    assert!(old.is_subset(&revoked), "the old grants are revoked");
    let own = c
        .resources
        .find(&reader.id, kind::SCHEMA, "reader")
        .unwrap();
    assert!(
        !c.ask_with(&reader.id, &read(), &own, &before).is_allowed(),
        "an old grant no longer works"
    );
    assert!(
        c.ask(&reader.id, &read(), &own).is_allowed(),
        "the new ones do"
    );
}
