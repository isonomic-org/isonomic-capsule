use std::sync::atomic::{AtomicI64, Ordering};

use broker::TokenKey;
use isonomic_proto::{Capabilities, Manifest};
use registry::GrantStore;

use super::*;

#[derive(Default)]
struct Manual(AtomicI64);

impl Clock for Manual {
    fn now_unix(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn manifest(id: &str) -> Manifest {
    Manifest {
        id: id.into(),
        name: "Chat".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities::default(),
        schemas: vec![],
    }
}

fn approval(id: &str) -> ApprovalId {
    serde_json::from_value(serde_json::json!(id)).unwrap()
}

fn setup() -> (Arc<Apps>, Arc<Manual>, EngineScopes) {
    let apps = Arc::new(Apps::new(
        GrantStore::in_memory().unwrap(),
        TokenKey::fixed([9; 32]),
    ));
    let clock = Arc::new(Manual::default());
    let scopes = EngineScopes::new(Arc::clone(&apps), clock.clone());
    (apps, clock, scopes)
}

#[test]
fn a_token_resolves_to_its_first_party_app_and_a_forged_one_to_nothing() {
    let (apps, _, scopes) = setup();
    let token = apps.grant(&manifest("com.isonomic.chat")).unwrap();
    let caller = scopes
        .resolve(&CapabilityToken(token.as_str().to_string()))
        .unwrap();
    assert_eq!(
        caller,
        Caller::app("com.isonomic.chat", AppKind::FirstParty)
    );
    let forged = scopes.resolve(&CapabilityToken("tok-forged".into()));
    assert_eq!(forged, Err(HostError::Unauthorized));
}

#[test]
fn a_user_action_works_once_for_its_caller_and_approval_only() {
    let (_, _, scopes) = setup();
    let chat = Caller::app("com.isonomic.chat", AppKind::FirstParty);
    let notes = Caller::app("com.isonomic.notes", AppKind::FirstParty);
    let (a, b) = (approval("apr_1"), approval("apr_2"));

    let token = scopes.mint(&chat, &a).unwrap();
    assert!(scopes.verify_user_action(&chat, &token, &a));
    assert!(!scopes.verify_user_action(&chat, &token, &a), "used twice");

    let token = scopes.mint(&chat, &a).unwrap();
    assert!(
        !scopes.verify_user_action(&chat, &token, &b),
        "another approval"
    );
    assert!(
        !scopes.verify_user_action(&chat, &token, &a),
        "spent by the wrong try"
    );

    let token = scopes.mint(&chat, &a).unwrap();
    assert!(
        !scopes.verify_user_action(&notes, &token, &a),
        "another caller"
    );
    let shell = scopes.mint(&Caller::shell(), &a).unwrap();
    assert!(scopes.verify_user_action(&Caller::shell(), &shell, &a));
    let forged = UserActionToken("0".repeat(64));
    assert!(!scopes.verify_user_action(&chat, &forged, &a));
}

#[test]
fn a_user_action_expires() {
    let (_, clock, scopes) = setup();
    let chat = Caller::app("com.isonomic.chat", AppKind::FirstParty);
    let a = approval("apr_1");
    let token = scopes.mint(&chat, &a).unwrap();
    clock.0.store(USER_ACTION_TTL_SECS, Ordering::SeqCst);
    assert!(!scopes.verify_user_action(&chat, &token, &a));
    let fresh = scopes.mint(&chat, &a).unwrap();
    clock
        .0
        .fetch_add(USER_ACTION_TTL_SECS - 1, Ordering::SeqCst);
    assert!(scopes.verify_user_action(&chat, &fresh, &a));
}
