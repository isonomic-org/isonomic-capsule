// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Every app data call goes through the check (R7 step 4), at the service
//! the broker asks. Hermetic: the identity library's in-memory key store.
//!
//! The costs are measured with:
//!
//!     cargo test --release -p identity --test check -- --ignored --nocapture

use std::sync::Arc;
use std::time::Instant;

use broker::{Apps, Identity, TokenKey};
use capsule_core::ManualClock;
use identity::apps::AppIds;
use identity::{memory_backend, IdentityService, OpenBackend, Person};
use isonomic_proto::agents::{ListSessions, SessionId, SubscribeSession};
use isonomic_proto::manifest::{AgentsAccess, BlobAccess, Schema};
use isonomic_proto::{Call, Capabilities, ErrorCode, Manifest};
use registry::GrantStore;

const CAPSULE: &str = "dddddddddddddddddddddddddddddddd";

fn notes() -> Manifest {
    Manifest {
        id: "com.isonomic.notes".into(),
        name: "Notes".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            agents: Some(AgentsAccess {}),
            blobs: Some(BlobAccess::default()),
            ..Default::default()
        },
        schemas: vec![Schema {
            name: "notes".into(),
        }],
    }
}

fn service(root: &std::path::Path, backend: OpenBackend, approved: &Arc<Apps>) -> IdentityService {
    let person = Person::new(
        root,
        CAPSULE,
        backend,
        Arc::new(ManualClock::at(1_800_000_000)),
    );
    IdentityService::new(person).with_apps(Arc::clone(approved))
}

/// One backend shared by every service made from it, as one Mac's.
fn shared() -> impl Fn() -> OpenBackend {
    let keys: Arc<dyn id_keys::KeyStore> = Arc::new(id_keys::MemoryKeyStore::new());
    let secrets: Arc<dyn identity::Secrets> = Arc::new(identity::MemorySecrets::default());
    move || {
        let (k, s) = (Arc::clone(&keys), Arc::clone(&secrets));
        Box::new(move || {
            Ok(identity::Backend {
                keys: Arc::clone(&k),
                secrets: Arc::clone(&s),
            })
        })
    }
}

fn query() -> Call {
    Call::SqlQuery {
        sql: "select 1".into(),
        params: vec![],
        schema: None,
    }
}

fn blob_get() -> Call {
    Call::BlobsGet {
        handle: "blob_1".into(),
        offset: 0,
        length: None,
    }
}

fn subscribe() -> Call {
    Call::SessionsSubscribe(SubscribeSession {
        session: SessionId::from("ses_1"),
        from_seq: 0,
    })
}

fn approved(root: &std::path::Path, manifests: &[&Manifest]) -> Arc<Apps> {
    let apps = Arc::new(Apps::new(
        GrantStore::open(&root.join("registry.db")).unwrap(),
        TokenKey::fixed([3; 32]),
    ));
    for m in manifests {
        apps.grant(m).unwrap();
    }
    apps
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_approved_before_it_had_grants_keeps_working() {
    let dir = tempfile::tempdir().unwrap();
    let notes = notes();
    // Approved by an earlier build: no ID, no grants.
    let apps = approved(dir.path(), &[&notes]);
    let backend = shared();
    let svc = service(dir.path(), backend(), &apps);
    svc.check(&notes, &query())
        .await
        .expect("before the ID, the manifest's checks hold");
    svc.get().await.unwrap();
    assert_eq!(
        AppIds::new(dir.path()).grants(&notes.id).len(),
        1,
        "issued when the ID was made"
    );

    // A Capsule with an ID but no grants yet (made before grants existed).
    std::fs::remove_dir_all(dir.path().join("identity/grants")).unwrap();
    let later = service(dir.path(), backend(), &apps);
    for call in [query(), blob_get(), subscribe()] {
        later
            .check(&notes, &call)
            .await
            .unwrap_or_else(|e| panic!("{call:?}: {e:?}"));
    }
    assert_eq!(
        AppIds::new(dir.path()).grants(&notes.id).len(),
        1,
        "issued on its first call"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_grant_is_refused_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let notes = notes();
    let apps = approved(dir.path(), &[&notes]);
    let backend = shared();
    let svc = service(dir.path(), backend(), &apps);
    svc.get().await.unwrap();
    svc.check(&notes, &query()).await.unwrap();

    // Revoke the app's grants as a changed approval would, and keep them.
    let ids = identity::apps::grant_ids(&AppIds::new(dir.path()).grants(&notes.id));
    std::fs::write(dir.path().join("identity/revoked"), ids.join("\n") + "\n").unwrap();
    let restarted = service(dir.path(), backend(), &apps);
    let refused = restarted.check(&notes, &query()).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::CapabilityDenied);
    assert!(
        refused
            .message
            .contains("grants don't let com.isonomic.notes read this"),
        "{}",
        refused.message
    );
    // Calls the check does not cover are not refused by it.
    let list = Call::SessionsUnsubscribe { subscription: 1 };
    restarted.check(&notes, &list).await.unwrap();
    let _ = ListSessions::default();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_approval_after_a_restart_needs_no_key() {
    // A scratch Capsule's second start: the ID and the grants are in the
    // Capsule, but the in-memory keys of the first run are gone.
    let dir = tempfile::tempdir().unwrap();
    let notes = notes();
    let apps = approved(dir.path(), &[&notes]);
    let first = service(dir.path(), memory_backend(), &apps);
    let made = first.get().await.unwrap();
    assert!(made.created);
    first.granted(&notes).await.unwrap();

    let second = service(dir.path(), memory_backend(), &apps);
    let again = second.get().await.unwrap();
    assert_eq!(
        (again.id, again.created),
        (made.id, false),
        "the same ID, read from the Capsule"
    );
    second
        .granted(&notes)
        .await
        .expect("approving the same app again signs nothing");
    for call in [query(), blob_get(), subscribe()] {
        second
            .check(&notes, &call)
            .await
            .unwrap_or_else(|e| panic!("{call:?}: {e:?}"));
    }

    // A new app does need signing, and says plainly why it can't be.
    let mut reader = notes.clone();
    reader.id = "com.isonomic.reader".into();
    reader.schemas = vec![Schema {
        name: "reader".into(),
    }];
    apps.grant(&reader).unwrap();
    let refused = second.granted(&reader).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::IdentityUnavailable);
    assert!(
        refused
            .message
            .contains("doesn't have the key that signs for your Isonomic ID"),
        "{}",
        refused.message
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "measures; run in release with --nocapture"]
async fn what_a_check_costs() {
    let dir = tempfile::tempdir().unwrap();
    let notes = notes();
    let apps = approved(dir.path(), &[&notes]);
    let svc = service(dir.path(), memory_backend(), &apps);
    svc.get().await.unwrap();
    let send = Call::SessionsSend(isonomic_proto::agents::SendInput {
        session: SessionId::from("ses_1"),
        content: vec![],
        client_ref: None,
    });
    for (name, call) in [
        ("blobs.get", blob_get()),
        ("sql.query", query()),
        ("sessions.subscribe", subscribe()),
        ("sessions.send", send),
    ] {
        let first = Instant::now();
        svc.check(&notes, &call).await.unwrap();
        let cold = first.elapsed();
        let n = 10_000;
        let warm = Instant::now();
        for _ in 0..n {
            svc.check(&notes, &call).await.unwrap();
        }
        let each = warm.elapsed() / n;
        println!(
            "{name:20} first {:>8.1} µs, then {:>6.2} µs a call",
            cold.as_secs_f64() * 1e6,
            each.as_secs_f64() * 1e6
        );
    }
}
