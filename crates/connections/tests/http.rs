// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A remote tool server over HTTP, against the fake server and its fake
//! sign-in server on loopback: a pasted token, the browser sign-in with
//! discovery, registration and PKCE, a refresh after the token is turned
//! down, and a server that takes no registrations. Hermetic: no network.

mod fake;

use std::sync::Arc;
use std::time::Duration;

use connections::{Connections, MemoryVault, NewConnection, Options, Server, State, Vault};

async fn until(connections: &Connections, id: &str, state: State) {
    let wait = async {
        loop {
            if connections.get(id).map(|c| c.state) == Some(state) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .unwrap_or_else(|_| panic!("{id} never became {state:?}: {:?}", connections.get(id)));
}

fn remote(url: String, token: Option<&str>) -> NewConnection {
    NewConnection {
        name: "Notes".into(),
        key: Some("notes".into()),
        server: Server::Remote { url },
        token: token.map(str::to_string),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pasted_token_is_sent_as_a_bearer_and_tools_are_called() {
    let server = fake::start().await;
    server.state().accepted.push("pasted-1".into());
    let dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(MemoryVault::default());
    let list = Connections::open(dir.path(), vault.clone(), Options::default()).unwrap();
    let added = list.add(remote(server.url(), Some("pasted-1"))).unwrap();
    until(&list, &added.id, State::Ready).await;
    let tools = list.tools(&added.id).unwrap();
    let marks: Vec<_> = tools
        .iter()
        .map(|t| (t.name.as_str(), t.read_only))
        .collect();
    assert_eq!(marks, [("search", true), ("delete", false)]);

    let out = list
        .call(&added.id, "search", serde_json::json!({"q": "plans"}))
        .await
        .unwrap();
    assert_eq!(out.text, "found \"plans\"");
    assert_eq!(server.state().calls, [serde_json::json!({"q": "plans"})]);
    // Every request after `initialize` carried the session id.
    let seen = server.state().sessions_seen.clone();
    assert!(
        !seen.is_empty() && seen.iter().all(|s| s == "session-1"),
        "{seen:?}"
    );
    // The token is in the vault, never in the file.
    let file = std::fs::read_to_string(dir.path().join("connections.json")).unwrap();
    assert!(!file.contains("pasted-1"));
    assert!(vault.get(&format!("{}:token", added.id)).unwrap().is_some());

    list.remove(&added.id).await.unwrap();
    assert!(vault.get(&format!("{}:token", added.id)).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_sign_in_registers_finishes_and_refreshes() {
    let server = fake::start().await;
    let dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(MemoryVault::default());
    let list = Connections::open(dir.path(), vault.clone(), Options::default()).unwrap();
    let added = list.add(remote(server.url(), None)).unwrap();
    until(&list, &added.id, State::NeedsSignIn).await;

    let page = list.sign_in(&added.id).await.unwrap();
    let page = reqwest::Url::parse(&page).unwrap();
    assert_eq!(page.path(), "/auth/authorize");
    let query: std::collections::HashMap<_, _> = page.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "client-1");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["resource"], server.url());
    assert_eq!(query["scope"], "notes");
    let redirect = &query["redirect_uri"];
    assert!(redirect.starts_with("http://127.0.0.1:"), "{redirect}");
    server.state().challenge = Some(query["code_challenge"].clone());
    server.state().accepted.push("tok-1".into());

    // The person's browser: a return with another state is not this
    // sign-in's, then the right one finishes it.
    let browser = reqwest::Client::new();
    let stray = format!("{redirect}?code=code-1&state=someone-else");
    let page = browser
        .get(&stray)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("did not finish"));
    let back = format!("{redirect}?code=code-1&state={}", query["state"]);
    let page = browser
        .get(&back)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("signed in"));
    until(&list, &added.id, State::Ready).await;
    assert_eq!(list.get(&added.id).unwrap().tools, 2);
    let kept = vault
        .get(&format!("{}:sign-in", added.id))
        .unwrap()
        .unwrap();
    assert!(String::from_utf8(kept).unwrap().contains("ref-1"));

    // The server turns the first token down: the call refreshes once.
    server.state().accepted = vec!["tok-2".into()];
    let out = list
        .call(&added.id, "search", serde_json::json!({"q": "after"}))
        .await
        .unwrap();
    assert_eq!(out.text, "found \"after\"");
    assert_eq!(server.state().refreshed, 1);
    let kept = vault
        .get(&format!("{}:sign-in", added.id))
        .unwrap()
        .unwrap();
    assert!(String::from_utf8(kept).unwrap().contains("tok-2"));

    // A new engine reconnects with the kept sign-in.
    drop(list);
    let again = Connections::open(dir.path(), vault, Options::default()).unwrap();
    until(&again, &added.id, State::Ready).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_takes_no_registration_cannot_be_signed_in_to() {
    let server = fake::start().await;
    server.state().no_registration = true;
    let dir = tempfile::tempdir().unwrap();
    let list = Connections::open(
        dir.path(),
        Arc::new(MemoryVault::default()),
        Options::default(),
    )
    .unwrap();
    let added = list.add(remote(server.url(), None)).unwrap();
    until(&list, &added.id, State::NeedsSignIn).await;
    let error = list.sign_in(&added.id).await.unwrap_err();
    assert!(error.contains("ahead of time"), "{error}");
    // Plain HTTP is refused anywhere but this computer.
    let far = remote("http://tools.example.com/api".into(), None);
    assert!(list.add(far).is_err());
}
