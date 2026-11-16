//! The directory of accepted logs, kept across restarts: new and longer
//! logs are kept, older copies change nothing, forks are refused and kept
//! for the person, and a stored log that no longer verifies is set aside.
//! Hermetic: logs are made with the identity library's in-memory store.

use std::path::Path;
use std::sync::Arc;

use capsule_core::ManualClock;
use id_core::log::{Commitment, Kind, Log, Op};
use id_core::{KeyAlg, KeyResolver, Signed, Signer};
use id_eth::EthKey;
use id_keys::{KeyStore, MemoryKeyStore, StoredKey};
use identity::directory::{Accepted, Logs, Refused};
use identity::{memory_backend, Person};

struct Keys(Arc<dyn KeyStore>);

impl Keys {
    fn new() -> Self {
        Self(Arc::new(MemoryKeyStore::new()))
    }
    fn make(&self, label: &str) -> StoredKey {
        StoredKey::generate(Arc::clone(&self.0), KeyAlg::P256, label).unwrap()
    }
}

/// A person whose first entry commits to device key `r1` and recovery
/// key `k1`.
fn person(keys: &Keys) -> (Log, StoredKey, StoredKey) {
    let (d1, r1, k1) = (keys.make("d1"), keys.make("r1"), keys.make("k1"));
    let next = vec![
        Commitment::device(&r1.public_key()),
        Commitment::recovery(&k1.public_key()),
    ];
    let log = Log::create(
        Kind::Person,
        None,
        vec![Op::add(d1.public_key())],
        next,
        100,
        &d1,
    )
    .unwrap();
    (log, r1, k1)
}

/// `log` continued by `signer`, adding a new device.
fn continued(log: &Log, keys: &Keys, signer: &StoredKey, recovery: &StoredKey, tag: &str) -> Log {
    let mut log = log.clone();
    let (d, r) = (
        keys.make(&format!("d-{tag}")),
        keys.make(&format!("r-{tag}")),
    );
    let next = vec![
        Commitment::device(&r.public_key()),
        Commitment::recovery(&recovery.public_key()),
    ];
    log.append(vec![Op::add(d.public_key())], next, 200, signer, &[])
        .unwrap();
    log
}

fn entries(log: &Log) -> Vec<Signed> {
    log.entries().to_vec()
}

fn kept(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("identity/logs"))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn an_accepted_log_is_kept_and_comes_back_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Keys::new();
    let (log, _, _) = person(&keys);
    let logs = Logs::open(dir.path());
    assert_eq!(logs.accept(&entries(&log)), Ok(Accepted::Newer));
    let digest = log
        .id()
        .as_str()
        .strip_prefix("did:iso:")
        .unwrap()
        .to_string();
    assert_eq!(kept(dir.path()), [format!("{digest}.log")]);

    let again = Logs::open(dir.path());
    assert!(again.notes().is_empty(), "{:?}", again.notes());
    let state = again.state(log.id()).expect("known after a restart");
    assert_eq!(state.seq, 0);
    let device = log.state().keys.keys().next().unwrap().clone();
    assert!(again.is_current_key(log.id(), &device, 100));
    assert!(again.version() > 0);
}

#[test]
fn a_longer_log_replaces_it_and_an_older_copy_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Keys::new();
    let (log, r1, k1) = person(&keys);
    let longer = continued(&log, &keys, &r1, &k1, "a");
    let logs = Logs::open(dir.path());
    logs.accept(&entries(&log)).unwrap();
    let before = logs.version();
    assert_eq!(logs.accept(&entries(&longer)), Ok(Accepted::Newer));
    assert!(logs.version() > before, "the version moves on a change");
    let moved = logs.version();
    assert_eq!(logs.accept(&entries(&log)), Ok(Accepted::Known));
    assert_eq!(logs.version(), moved, "an older copy changes nothing");
    assert_eq!(Logs::open(dir.path()).state(log.id()).unwrap().seq, 1);
}

#[test]
fn a_fork_is_refused_kept_for_the_person_and_the_known_log_stays() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Keys::new();
    let (log, r1, k1) = person(&keys);
    let by_device = continued(&log, &keys, &r1, &k1, "a");
    // The recovery kit signs a different second entry.
    let by_kit = continued(&log, &keys, &k1, &keys.make("k2"), "b");
    let logs = Logs::open(dir.path());
    logs.accept(&entries(&by_device)).unwrap();
    let version = logs.version();

    let Err(Refused::Fork(fork)) = logs.accept(&entries(&by_kit)) else {
        panic!("a fork was not refused");
    };
    assert_eq!(
        (fork.id.clone(), fork.at, fork.by_recovery),
        (log.id().clone(), 1, true)
    );
    assert!(fork.file.starts_with(dir.path().join("identity/forks")));
    assert_eq!(
        identity_file_entries(&fork.file),
        entries(&by_kit).len(),
        "the competing log is kept whole"
    );
    assert_eq!(logs.forks(), [fork]);
    assert_eq!(logs.version(), version, "nothing switched");
    let known = Logs::open(dir.path()).state(log.id()).unwrap();
    assert_eq!(known.head, by_device.state().head, "the known branch stays");
}

fn identity_file_entries(path: &Path) -> usize {
    std::fs::read_to_string(path).unwrap().lines().count()
}

#[test]
fn a_kept_log_that_no_longer_verifies_is_set_aside_and_said() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Keys::new();
    let (good, _, _) = person(&keys);
    let (other, _, _) = person(&Keys::new());
    let logs = Logs::open(dir.path());
    logs.accept(&entries(&good)).unwrap();
    logs.accept(&entries(&other)).unwrap();
    let broken = dir
        .path()
        .join("identity/logs")
        .join(kept(dir.path())[0].clone());
    std::fs::write(&broken, "00ff\n").unwrap();

    let again = Logs::open(dir.path());
    assert_eq!(again.notes().len(), 1, "{:?}", again.notes());
    assert!(again.notes()[0].contains("moved to identity/unreadable"));
    let name = broken.file_name().unwrap();
    assert!(
        dir.path().join("identity/unreadable").join(name).exists(),
        "kept, not deleted"
    );
    let known = [&good, &other]
        .iter()
        .filter(|l| again.state(l.id()).is_some())
        .count();
    assert_eq!(known, 1, "the other log still loads");
}

#[test]
fn a_log_with_an_ethereum_binding_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Keys::new();
    let (mut log, r1, k1) = person(&keys);
    let r2 = keys.make("r2");
    let next = vec![
        Commitment::device(&r2.public_key()),
        Commitment::recovery(&k1.public_key()),
    ];
    let bind = vec![Op::Bind(EthKey::from_seed(&[9; 32]).bind(log.id()))];
    log.append(bind, next, 200, &r1, &identity::directory::checks())
        .unwrap();
    let logs = Logs::open(dir.path());
    assert_eq!(logs.accept(&entries(&log)), Ok(Accepted::Newer));
    assert!(Logs::open(dir.path())
        .state(log.id())
        .unwrap()
        .bindings
        .contains_key("eth"));
}

#[test]
fn the_persons_own_log_is_in_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::at(1_800_000_000));
    let person = Person::new(dir.path(), &"a".repeat(32), memory_backend(), clock);
    let (id, _) = person.get().unwrap();
    assert!(person.logs().state(&id).is_some());
    assert!(
        Logs::open(dir.path()).state(&id).is_some(),
        "kept for a restart"
    );
}
