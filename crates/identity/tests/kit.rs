//! The recovery kit, end to end at the service the broker asks: export on
//! one Mac, restore on another (a fresh Capsule and a fresh key store), the
//! same ID, a new app approval that works, and the old keys retired. Every
//! refusal, the spent kit, and an older kit after a device rotation.
//! Hermetic: the identity library's in-memory key stores.

use std::path::Path;
use std::sync::Arc;

use broker::{Apps, Identity, TokenKey};
use capsule_core::ManualClock;
use id_core::log::{Commitment, Directory, Log, Op, Role};
use id_core::KeyAlg;
use id_keys::{KeyHandle, KeyStore, MemoryKeyStore, StoredKey};
use identity::kit::{Kit, KitError, RecoveryError};
use identity::{Backend, IdentityService, MemorySecrets, OpenBackend, Person, Secrets};
use isonomic_proto::manifest::{BlobAccess, Schema};
use isonomic_proto::{Call, Capabilities, Manifest};
use registry::GrantStore;

const CAPSULE: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// One Mac: its Capsule folder, its key store, its engine's service.
struct Mac {
    dir: tempfile::TempDir,
    keys: Arc<dyn KeyStore>,
    apps: Arc<Apps>,
    service: IdentityService,
}

fn mac() -> Mac {
    let dir = tempfile::tempdir().unwrap();
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::new());
    let secrets: Arc<dyn Secrets> = Arc::new(MemorySecrets::default());
    let (k, s) = (Arc::clone(&keys), Arc::clone(&secrets));
    let backend: OpenBackend = Box::new(move || {
        Ok(Backend {
            keys: Arc::clone(&k),
            secrets: Arc::clone(&s),
        })
    });
    let apps = Arc::new(Apps::new(
        GrantStore::open(&dir.path().join("registry.db")).unwrap(),
        TokenKey::fixed([5; 32]),
    ));
    let person = Person::new(
        dir.path(),
        CAPSULE,
        backend,
        Arc::new(ManualClock::at(1_800_000_000)),
    );
    let service = IdentityService::new(person).with_apps(Arc::clone(&apps));
    Mac {
        dir,
        keys,
        apps,
        service,
    }
}

fn notes() -> Manifest {
    Manifest {
        id: "com.isonomic.notes".into(),
        name: "Notes".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            blobs: Some(BlobAccess::default()),
            ..Default::default()
        },
        schemas: vec![Schema {
            name: "notes".into(),
        }],
    }
}

fn query() -> Call {
    Call::SqlQuery {
        sql: "select 1".into(),
        params: vec![],
        schema: None,
    }
}

impl Mac {
    async fn approve(&self, manifest: &Manifest) {
        self.apps.grant(manifest).unwrap();
        self.service.granted(manifest).await.unwrap();
    }
}

fn log_of(document: &str) -> Log {
    Kit::from_document(document).unwrap().open().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kit_gives_the_same_id_on_a_new_mac_and_retires_the_old_keys() {
    let old = mac();
    let id = old.service.get().await.unwrap().id;
    old.approve(&notes()).await;
    assert!(matches!(
        old.service.kit_export(false),
        Err(RecoveryError::NotAsked)
    ));
    let kit = old.service.kit_export(true).unwrap();
    assert_eq!(kit.id, id);
    assert_eq!(kit.words.len(), 24);
    assert!(old.service.kit_status().exported_at.is_some());

    // The Mac is lost. A new one, empty, with the words typed from paper.
    let new = mac();
    let restored = new
        .service
        .kit_restore(&kit.document, &kit.words, false)
        .unwrap();
    assert_eq!(restored.id, id, "the same ID");
    assert_eq!(new.service.get().await.unwrap().id, id);
    assert_ne!(
        restored.words, kit.words,
        "the old kit is spent; a new one is given"
    );
    let status = new.service.kit_status();
    assert!(status.saved_at.is_none(), "the new kit is not saved yet");

    // The old device's signing key no longer signs for the ID; a new one does.
    let before = log_of(&kit.document);
    let after = log_of(&restored.document);
    let old_keys: Vec<_> = before.state().keys.keys().cloned().collect();
    let mut directory = Directory::default();
    directory.accept(after.entries(), &[]).unwrap();
    use id_core::KeyResolver;
    for key in &old_keys {
        assert!(!directory.is_current_key(after.id(), key, 1_800_000_000));
    }
    assert_eq!(after.state().keys.len(), 1);
    assert!(!old_keys.contains(after.state().keys.keys().next().unwrap()));

    // A new app approval on the new Mac works through the check.
    new.approve(&notes()).await;
    new.service.check(&notes(), &query()).await.unwrap();

    // The person saves the new kit.
    new.service.kit_saved().unwrap();
    assert!(new.service.kit_status().saved_at.is_some());

    // The spent kit's words, with the new kit's record, are refused plainly.
    let refused = new_mac_restore(&restored.document, &kit.words);
    assert!(
        matches!(refused, Err(RecoveryError::Kit(KitError::Spent))),
        "{refused:?}"
    );
}

fn new_mac_restore(document: &str, words: &[String]) -> Result<String, RecoveryError> {
    mac()
        .service
        .kit_restore(document, words, false)
        .map(|r| r.id)
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_words_damaged_kits_and_other_ids_are_refused_plainly() {
    let one = mac();
    one.service.get().await.unwrap();
    let kit = one.service.kit_export(true).unwrap();
    let other = mac();
    other.service.get().await.unwrap();
    let other_kit = other.service.kit_export(true).unwrap();

    let mut swapped = kit.words.clone();
    swapped.swap(0, 1);
    let refused = new_mac_restore(&kit.document, &swapped).unwrap_err();
    assert!(
        matches!(refused, RecoveryError::Kit(KitError::Words(_))),
        "{refused:?}"
    );
    assert!(
        refused.to_string().contains("mistyped or out of order"),
        "{refused}"
    );

    let refused = new_mac_restore(&kit.document, &other_kit.words).unwrap_err();
    assert!(
        matches!(refused, RecoveryError::Kit(KitError::NotThisKit)),
        "{refused:?}"
    );

    let damaged = kit.document.replacen("\"made\": ", "\"made\": 1", 1);
    let refused = new_mac_restore(&damaged, &kit.words).unwrap_err();
    assert!(
        matches!(refused, RecoveryError::Kit(KitError::Damaged)),
        "{refused:?}"
    );

    let refused = new_mac_restore("{\"format\": \"something else\"}", &kit.words).unwrap_err();
    assert!(
        matches!(refused, RecoveryError::Kit(KitError::NotAKit)),
        "{refused:?}"
    );

    // A Mac whose own ID holds apps never has it replaced.
    one.approve(&notes()).await;
    let refused = one
        .service
        .kit_restore(&other_kit.document, &other_kit.words, true)
        .unwrap_err();
    assert!(
        matches!(refused, RecoveryError::Kit(KitError::InUse)),
        "{refused:?}"
    );

    // A Mac whose ID is unused keeps it unless asked, and sets it aside if so.
    let fresh = mac();
    fresh.service.get().await.unwrap();
    let refused = fresh
        .service
        .kit_restore(&kit.document, &kit.words, false)
        .unwrap_err();
    assert!(
        matches!(refused, RecoveryError::Kit(KitError::HasId)),
        "{refused:?}"
    );
    let restored = fresh
        .service
        .kit_restore(&kit.document, &kit.words, true)
        .unwrap();
    assert_eq!(restored.id, kit.id);
    let aside = std::fs::read_dir(fresh.dir.path().join("identity"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("replaced-"))
        .count();
    assert_eq!(aside, 1, "the unused ID is set aside, never deleted");

    // No kit before there is an ID.
    assert!(matches!(
        mac().service.kit_export(true),
        Err(RecoveryError::NoId)
    ));
}

/// A device rotation on the old Mac, after its kit was made: a new signing
/// key, a new device rotation key, the same recovery commitment.
fn rotate(old: &Mac) {
    let path = old.dir.path().join("identity").join("person.log");
    let entries = identity_files_read(&path);
    let mut log = Log::open(entries, &[]).unwrap();
    let label = format!("capsule/{CAPSULE}/person/device-rotation");
    let rotation = StoredKey::open(Arc::clone(&old.keys), KeyHandle(label)).unwrap();
    let signing =
        StoredKey::generate(Arc::clone(&old.keys), KeyAlg::P256, "test/signing-2").unwrap();
    let next_rotation =
        StoredKey::generate(Arc::clone(&old.keys), KeyAlg::P256, "test/rotation-2").unwrap();
    let recovery = *log
        .state()
        .next
        .iter()
        .find(|c| c.role == Role::Recovery)
        .unwrap();
    use id_core::Signer;
    log.append(
        vec![Op::add(signing.public_key())],
        vec![Commitment::device(&next_rotation.public_key()), recovery],
        1_800_000_100,
        &rotation,
        &[],
    )
    .unwrap();
    let mut text = String::new();
    for e in log.entries() {
        let bytes = e.to_bytes().unwrap();
        text.push_str(&bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
}

fn identity_files_read(path: &Path) -> Vec<id_core::Signed> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| {
            let bytes: Vec<u8> = (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap())
                .collect();
            id_core::Signed::from_bytes(&bytes).unwrap()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_kit_still_restores_and_the_lost_macs_later_log_is_kept_apart() {
    let old = mac();
    let id = old.service.get().await.unwrap().id;
    let kit = old.service.kit_export(true).unwrap();
    rotate(&old);

    let new = mac();
    let restored = new
        .service
        .kit_restore(&kit.document, &kit.words, false)
        .unwrap();
    assert_eq!(restored.id, id, "an older kit restores the same ID");

    // The lost Mac's longer log turns up: it parts ways at entry 1. It is
    // kept apart and never adopted; the restored log stays in force.
    let longer = identity_files_read(&old.dir.path().join("identity").join("person.log"));
    let logs = identity::directory::Logs::open(new.dir.path());
    assert!(logs.accept(&longer).is_err());
    let forks = logs.forks();
    assert_eq!(forks.len(), 1);
    assert_eq!(forks[0].at, 1);
    let reopened = identity::directory::Logs::open(new.dir.path());
    let known = log_of(&restored.document);
    assert_eq!(reopened.state(known.id()).unwrap().head, known.state().head);
}

/// Whether `text` holds any two of `words` in a row: everyday words of the
/// list (such as "kit") appear in ordinary text, a run of them does not.
fn leaks(text: &str, words: &[String]) -> bool {
    let lower = text.to_lowercase();
    words
        .windows(2)
        .any(|pair| lower.contains(&format!("{} {}", pair[0], pair[1])))
        || lower.contains(&words.join(" "))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_words_never_reach_the_file_the_log_an_error_or_a_debug_print() {
    let old = mac();
    old.service.get().await.unwrap();
    let kit = old.service.kit_export(true).unwrap();

    // The saved file holds no word of the seed, only its fingerprint.
    let doc: serde_json::Value = serde_json::from_str(&kit.document).unwrap();
    assert!(doc.get("words").is_none());
    assert_eq!(doc["recovery"].as_str().unwrap().len(), 16);
    for value in doc.as_object().unwrap().values() {
        if let Some(text) = value.as_str() {
            assert!(!kit.words.iter().any(|w| w == text), "{text}");
        }
    }
    assert!(!leaks(&kit.document, &kit.words));

    // Refusals never repeat what the person typed.
    let mut typed = kit.words.clone();
    typed[3] = "plantedmistake".into();
    let refused = new_mac_restore(&kit.document, &typed)
        .unwrap_err()
        .to_string();
    assert!(!refused.contains("plantedmistake"), "{refused}");
    assert!(refused.contains("Word 4"), "{refused}");
    let mut swapped = kit.words.clone();
    swapped.swap(5, 6);
    let refused = new_mac_restore(&kit.document, &swapped)
        .unwrap_err()
        .to_string();
    assert!(!leaks(&refused, &kit.words), "{refused}");

    // A restore, and what it says and keeps.
    let new = mac();
    let restored = new
        .service
        .kit_restore(&kit.document, &kit.words, false)
        .unwrap();
    assert!(
        restored.note.contains("destroy the old one"),
        "{}",
        restored.note
    );
    assert!(!leaks(&restored.document, &restored.words));

    // Nothing in the engine's log, and no debug print, holds either kit's
    // words: a panic message or a problem report could carry those.
    let log = capsule_core::log::tail().join("\n");
    for words in [&kit.words, &restored.words] {
        assert!(!leaks(&log, words), "{log}");
        assert!(!leaks(&format!("{kit:?} {restored:?}"), words));
    }
    assert!(format!("{kit:?}").contains("hidden"));
    assert!(log.contains("recovery kit exported"), "{log}");
    assert!(
        log.contains("should be destroyed once the new one is saved"),
        "{log}"
    );
}
