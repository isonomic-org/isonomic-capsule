// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The recovery kit through the whole engine, over its socket: export on one
//! Capsule, restore on a fresh one (its own key store), the same ID, and an
//! app approved after the restore working with grants from it. On the way,
//! every refusal is `recovery_refused` and never names back the words.

mod support;

use isonomic_client::{Client, ClientError};
use isonomic_inference_testkit::FakeModelServer;
use isonomic_proto::identity::{KitRestore, RecoveryWords};
use isonomic_proto::ErrorCode;

/// The refusal's message, after checking it is `recovery_refused`. Fixed
/// text: the same refusal with other words gives the same message, so
/// nothing typed is named back.
fn refused<T: std::fmt::Debug>(result: Result<T, ClientError>) -> String {
    match result {
        Err(ClientError::Engine(e)) => {
            assert_eq!(e.code, ErrorCode::RecoveryRefused, "{}", e.message);
            e.message
        }
        other => panic!("expected recovery_refused, got {other:?}"),
    }
}

/// Every string in the kit document, keys and values.
fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

fn restore(kit: &str, words: Vec<String>) -> KitRestore {
    KitRestore {
        kit: kit.to_string(),
        words: RecoveryWords::new(words),
        replace_unused: false,
    }
}

async fn chat_works(shell: &Client, id: &str) {
    let mut manifest = support::chat_manifest();
    manifest.id = id.into();
    let token = shell.grant(manifest).await.unwrap();
    let app = shell.scoped(token);
    app.agents()
        .list(Default::default())
        .await
        .expect("an app approved now works");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kit_from_one_capsule_restores_the_same_id_on_a_fresh_one() {
    let server = FakeModelServer::start().await;

    // The first Mac: an ID, an app, and the kit the person asked for.
    let old_dir = tempfile::tempdir().unwrap();
    let old = support::shell(old_dir.path(), server.port()).await;
    let made = old.identity().await.unwrap();
    chat_works(&old, "com.isonomic.chat").await;

    refused(old.kit_saved().await);
    refused(old.kit_export(false).await);
    let kit = old.kit_export(true).await.unwrap();
    assert_eq!(kit.id, made.id);
    let words = kit.words.reveal().to_vec();
    assert_eq!(words.len(), 24);
    let mut held = Vec::new();
    strings(&serde_json::from_str(&kit.kit).unwrap(), &mut held);
    assert!(
        held.iter().all(|s| !words.contains(s)),
        "the kit holds a word"
    );
    assert!(
        !format!("{kit:?}").contains(&words[..3].join("\", \"")),
        "Debug shows the words"
    );
    let status = old.kit_status().await.unwrap();
    assert!(status.exported_at.is_some() && status.saved_at.is_none());
    old.kit_saved().await.unwrap();
    assert!(old.kit_status().await.unwrap().saved_at.is_some());

    // A fresh Mac, with its own key store and no ID yet.
    let new_dir = tempfile::tempdir().unwrap();
    let new = support::shell(new_dir.path(), server.port()).await;
    assert_eq!(new.kit_status().await.unwrap(), Default::default());

    // Words that are not the kit's, a word that is not on the list, and a
    // file that is not a kit are refused without changing anything.
    let other = identity::words::encode(&[7; 32]);
    let another = identity::words::encode(&[8; 32]);
    let not_this = refused(new.kit_restore(restore(&kit.kit, other.clone())).await);
    let not_that = refused(new.kit_restore(restore(&kit.kit, another.clone())).await);
    assert_eq!(not_this, not_that);
    let typo = |mut w: Vec<String>| {
        w[4] = "zzzzq".into();
        w
    };
    let said = refused(
        new.kit_restore(restore(&kit.kit, typo(words.clone())))
            .await,
    );
    let again = refused(
        new.kit_restore(restore(&kit.kit, typo(other.clone())))
            .await,
    );
    assert_eq!(said, again);
    assert!(said.contains('5'), "names the position: {said}");
    let planted = refused(
        new.kit_restore(restore(&words.join(" "), words.clone()))
            .await,
    );
    let other_planted = refused(
        new.kit_restore(restore(&other.join(" "), words.clone()))
            .await,
    );
    assert_eq!(planted, other_planted);

    // The kit and its words: the same ID, and a new kit replacing it.
    let restored = new
        .kit_restore(restore(&kit.kit, words.clone()))
        .await
        .unwrap();
    assert_eq!(restored.id, made.id);
    assert_ne!(
        restored.words.reveal(),
        words.as_slice(),
        "the kit is replaced"
    );
    assert!(!restored.note.is_empty());
    let again = new.identity().await.unwrap();
    assert_eq!(
        (again.id.as_str(), again.created),
        (made.id.as_str(), false)
    );
    let status = new.kit_status().await.unwrap();
    assert!(status.exported_at.is_some() && status.saved_at.is_none());

    // An app approved on the new Mac gets its grants from the restored ID.
    chat_works(&new, "com.isonomic.chat").await;
    let grants = identity::apps::AppIds::new(new_dir.path()).grants("com.isonomic.chat");
    assert_eq!(grants.len(), 1, "Chat's space grant");

    // The same kit again: this Mac has the ID now.
    refused(new.kit_restore(restore(&kit.kit, words.clone())).await);
}
