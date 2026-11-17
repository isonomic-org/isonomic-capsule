// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The person's ID: made once, then the same one, from what the Capsule
//! and the key store keep. Hermetic: the identity library's in-memory key
//! store and in-memory secrets stand in for the Mac's.

use std::sync::Arc;

use capsule_core::ManualClock;
use id_core::log::{replay, Role};
use id_core::{KeyAlg, Signed};
use identity::{
    memory_backend, Backend, IdentityError, MemorySecrets, OpenBackend, Person, Secrets,
    NO_KEY_STORE,
};

const CAPSULE: &str = "0123456789abcdef0123456789abcdef";

fn person(root: &std::path::Path, open: OpenBackend) -> Person {
    Person::new(
        root,
        CAPSULE,
        open,
        Arc::new(ManualClock::at(1_800_000_000)),
    )
}

/// One backend shared by every `Person` made from it, as one Mac's.
fn shared() -> (
    Arc<dyn id_keys::KeyStore>,
    Arc<MemorySecrets>,
    impl Fn() -> OpenBackend,
) {
    let keys: Arc<dyn id_keys::KeyStore> = Arc::new(id_keys::MemoryKeyStore::new());
    let secrets = Arc::new(MemorySecrets::default());
    let (k, s) = (Arc::clone(&keys), Arc::clone(&secrets));
    let open = move || -> OpenBackend {
        let (k, s) = (Arc::clone(&k), Arc::clone(&s));
        Box::new(move || {
            Ok(Backend {
                keys: Arc::clone(&k),
                secrets: s.clone() as Arc<dyn Secrets>,
            })
        })
    };
    (keys, secrets, open)
}

fn entries(root: &std::path::Path) -> Vec<Signed> {
    let text = std::fs::read_to_string(root.join("identity/person.log")).unwrap();
    text.lines()
        .map(|l| {
            let bytes: Vec<u8> = (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap())
                .collect();
            Signed::from_bytes(&bytes).unwrap()
        })
        .collect()
}

#[test]
fn the_id_is_made_once_and_the_same_one_comes_back() {
    let dir = tempfile::tempdir().unwrap();
    let (_, _, open) = shared();
    let first = person(dir.path(), open());
    let (id, created) = first.get().unwrap();
    assert!(created);
    let text = id.as_str();
    assert!(text.starts_with("did:iso:") && text.len() == 72, "{text}");
    assert_eq!(first.get().unwrap(), (id.clone(), false));
    // A later engine on the same Capsule reads it back without the store.
    let refusing: OpenBackend = Box::new(|| Err("not asked".into()));
    assert_eq!(person(dir.path(), refusing).get().unwrap(), (id, false));
}

#[test]
fn the_log_commits_a_device_key_and_the_recovery_key_of_the_kept_seed() {
    let dir = tempfile::tempdir().unwrap();
    let (keys, secrets, open) = shared();
    let (id, _) = person(dir.path(), open()).get().unwrap();
    let state = replay(&entries(dir.path()), &[]).unwrap();
    assert_eq!(state.seq, 0);
    let signing = keys
        .public_key(&id_keys::KeyHandle(format!(
            "capsule/{CAPSULE}/person/device-signing"
        )))
        .unwrap();
    assert_eq!(signing.alg, KeyAlg::P256);
    assert!(state.keys.contains_key(&signing), "the device signs for it");
    let seed: [u8; 32] = secrets
        .get(&format!("capsule/{CAPSULE}/person/recovery-seed"))
        .unwrap()
        .expect("the seed is kept")
        .try_into()
        .unwrap();
    let recovery = identity::recovery::recovery_public_key(&seed).fingerprint();
    let roles: Vec<(Role, bool)> = state
        .next
        .iter()
        .map(|c| (c.role, c.key == recovery))
        .collect();
    assert!(roles.contains(&(Role::Recovery, true)), "{roles:?}");
    assert!(roles.iter().any(|(r, _)| *r == Role::Device), "{roles:?}");
    assert!(id.as_str().len() == 72);
}

#[test]
fn a_crash_before_the_log_was_written_uses_what_was_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (_, _, open) = shared();
    let (first, _) = person(dir.path(), open()).get().unwrap();
    let signer = entries(dir.path())[0].signer().clone();
    // The log is lost after the keys and seed were kept.
    std::fs::remove_file(dir.path().join("identity/person.log")).unwrap();
    let (again, created) = person(dir.path(), open()).get().unwrap();
    assert!(created);
    assert_eq!(
        entries(dir.path())[0].signer(),
        &signer,
        "the same device key"
    );
    // A new log means a new first entry, so a new ID: the old one was never
    // handed out by this Capsule's record.
    assert_ne!(first, again);
}

#[test]
fn without_a_key_store_the_reason_is_said_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let none: OpenBackend = Box::new(|| Err(NO_KEY_STORE.to_string()));
    let refused = person(dir.path(), none).get().unwrap_err();
    assert_eq!(refused, IdentityError::Unavailable(NO_KEY_STORE.into()));
    assert!(!dir.path().join("identity").exists());
}

#[test]
fn a_refused_key_store_is_said_in_plain_words() {
    struct Locked;
    impl Secrets for Locked {
        fn get(&self, _: &str) -> Result<Option<Vec<u8>>, String> {
            Err("User interaction is not allowed.".into())
        }
        fn put(&self, _: &str, _: &[u8]) -> Result<(), String> {
            Err("User interaction is not allowed.".into())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let locked: OpenBackend = Box::new(|| {
        Ok(Backend {
            keys: Arc::new(id_keys::MemoryKeyStore::new()),
            secrets: Arc::new(Locked),
        })
    });
    let refused = person(dir.path(), locked).get().unwrap_err().to_string();
    assert!(
        refused.starts_with("Your computer's key store didn't let Isonomic make your ID"),
        "{refused}"
    );
    assert!(refused.contains("unlock it, then try again"), "{refused}");
    assert!(!dir.path().join("identity/person.log").exists());
}

#[test]
fn a_damaged_record_is_refused_and_left_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("identity")).unwrap();
    std::fs::write(dir.path().join("identity/person.log"), "not hex\n").unwrap();
    let refused = person(dir.path(), memory_backend()).get().unwrap_err();
    assert!(
        matches!(refused, IdentityError::Unreadable(_)),
        "{refused:?}"
    );
    let kept = std::fs::read_to_string(dir.path().join("identity/person.log")).unwrap();
    assert_eq!(kept, "not hex\n");
}
