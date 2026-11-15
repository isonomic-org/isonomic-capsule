use isonomic_proto::manifest::ModelAccess;
use isonomic_proto::Capabilities;

use super::*;

fn manifest() -> Manifest {
    Manifest {
        id: "com.isonomic.hello".into(),
        name: "Hello".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities::default(),
        schemas: vec![],
    }
}

fn apps(key: u8) -> Apps {
    Apps::new(GrantStore::in_memory().unwrap(), TokenKey::fixed([key; 32]))
}

#[test]
fn hmac_matches_the_published_test_vector() {
    // RFC 4231, test case 2, with the key zero-padded to 32 bytes: HMAC
    // pads short keys with zeros, so the result is the same.
    let mut key = [0u8; 32];
    key[..4].copy_from_slice(b"Jefe");
    let mac = TokenKey::fixed(key).mac(b"what do ya want for nothing?");
    assert_eq!(
        hex(&mac),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

#[test]
fn a_grant_yields_a_token_that_finds_the_manifest() {
    let apps = apps(1);
    let token = apps.grant(&manifest()).unwrap();
    assert_eq!(apps.manifest(&token).unwrap(), manifest());
    assert!(!token.as_str().contains("hello"), "tokens are opaque");
    let forged = Token::new("tok-0000");
    assert_eq!(
        apps.manifest(&forged).unwrap_err().code,
        ErrorCode::UnknownToken
    );
}

#[test]
fn the_same_approval_gives_the_same_token_under_the_same_key_only() {
    let first = apps(1).grant(&manifest()).unwrap();
    assert_eq!(apps(1).grant(&manifest()).unwrap(), first);
    assert_ne!(apps(2).grant(&manifest()).unwrap(), first);

    let mut wider = manifest();
    wider.capabilities.models = Some(ModelAccess {
        any: true,
        ..Default::default()
    });
    assert_ne!(apps(1).grant(&wider).unwrap(), first);
}

#[test]
fn granting_again_keeps_earlier_tokens_on_the_new_manifest() {
    let apps = apps(1);
    let old = apps.grant(&manifest()).unwrap();
    let mut wider = manifest();
    wider.version = "0.2.0".into();
    wider.capabilities.models = Some(ModelAccess::default());
    let new = apps.grant(&wider).unwrap();
    assert_ne!(old, new);
    assert_eq!(apps.manifest(&old).unwrap(), wider);
    assert_eq!(apps.manifest(&new).unwrap(), wider);
}

#[test]
fn an_invalid_manifest_is_refused_and_nothing_is_recorded() {
    let apps = apps(1);
    let mut bad = manifest();
    bad.version = "1.2".into();
    let error = apps.grant(&bad).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidManifest);
    // The refused manifest's would-be token was never stored.
    assert_eq!(
        apps.manifest(&apps.token_for(&bad)).unwrap_err().code,
        ErrorCode::UnknownToken
    );
}

#[test]
fn the_key_is_created_once_user_only_and_then_reused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets/token.key");
    let first = TokenKey::load_or_create(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let again = TokenKey::load_or_create(&path).unwrap();
    assert_eq!(first.0, again.0);
    assert_eq!(format!("{first:?}"), "TokenKey(..)");

    std::fs::write(&path, b"short").unwrap();
    assert!(TokenKey::load_or_create(&path).is_err());
}

#[test]
fn tokens_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("registry.db");
    let key = dir.path().join("secrets/token.key");
    let open = || {
        Apps::new(
            GrantStore::open(&db).unwrap(),
            TokenKey::load_or_create(&key).unwrap(),
        )
    };
    let token = open().grant(&manifest()).unwrap();
    let restarted = open();
    assert_eq!(restarted.manifest(&token).unwrap(), manifest());
}

#[test]
fn the_shells_own_app_id_is_never_granted() {
    let apps = apps(1);
    let mut shell = manifest();
    shell.id = RESERVED_SHELL_ID.into();
    let error = apps.grant(&shell).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidManifest);
    assert!(error.message.contains("reserved"), "{}", error.message);
}
