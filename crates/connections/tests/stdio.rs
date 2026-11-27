// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A local tool server over standard input and output: a stand-in script
//! that answers `initialize`, `tools/list` and `tools/call` line by line.
//! It starts with the allow-listed environment only, its tools are listed
//! with their read-only hints, a call's text comes back, a reopened list
//! reconnects, and a removed connection is forgotten. Hermetic: no network.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use connections::{Connections, MemoryVault, NewConnection, Options, Server, State};

const SERVER: &str = r#"#!/bin/sh
env > "$0.env"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"stand-in","version":"1"}}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"find_notes","description":"Find notes.","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}},{"name":"send_note","description":"Send a note.","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","method":"notifications/progress","params":{}}\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"read from stdio"}]}}\n' "$id" ;;
  esac
done
"#;

fn script(dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("stand-in.sh");
    std::fs::write(&path, SERVER).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

fn options() -> Options {
    Options {
        child_env: vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("ISONOMIC_STAND_IN".into(), "yes".into()),
        ],
        ..Options::default()
    }
}

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

#[tokio::test(flavor = "multi_thread")]
async fn a_local_server_starts_lists_calls_and_comes_back_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let command = script(dir.path());
    let vault = Arc::new(MemoryVault::default());
    let list = Connections::open(dir.path(), vault.clone(), options()).unwrap();
    let mut changes = list.subscribe();
    let added = list
        .add(NewConnection {
            name: "Notes".into(),
            key: Some("notes".into()),
            server: Server::Local {
                command: "/bin/sh".into(),
                args: vec![command.clone()],
            },
            token: None,
        })
        .unwrap();
    // Persisted before it was published.
    let file = std::fs::read_to_string(dir.path().join("connections.json")).unwrap();
    assert!(file.contains(&added.id));
    assert_eq!(changes.recv().await.unwrap().id, added.id);
    until(&list, &added.id, State::Ready).await;
    let tools = list.tools(&added.id).unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|t| (t.name.as_str(), t.read_only))
        .collect();
    assert_eq!(names, [("find_notes", true), ("send_note", false)]);
    assert_eq!(list.get(&added.id).unwrap().tools, 2);

    let out = list
        .call(&added.id, "find_notes", serde_json::json!({"q": "plans"}))
        .await
        .unwrap();
    assert_eq!(out.text, "read from stdio");
    assert!(!out.is_error);
    let refused = list
        .call(&added.id, "no_such_tool", serde_json::json!({}))
        .await;
    assert!(refused.unwrap_err().contains("no tool no_such_tool"));

    // The child saw the allow-listed environment and nothing else.
    let env = std::fs::read_to_string(format!("{command}.env")).unwrap();
    assert!(env.contains("ISONOMIC_STAND_IN=yes"), "{env}");
    assert!(!env.lines().any(|l| l.starts_with("HOME=")), "{env}");

    // A new engine on the same folder reconnects it.
    drop(list);
    let again = Connections::open(dir.path(), vault, options()).unwrap();
    until(&again, &added.id, State::Ready).await;
    again.remove(&added.id).await.unwrap();
    assert!(again.get(&added.id).is_none());
    assert!(again.remove(&added.id).await.is_err());
    let file = std::fs::read_to_string(dir.path().join("connections.json")).unwrap();
    assert!(!file.contains(&added.id));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_does_not_start_fails_in_plain_words() {
    let dir = tempfile::tempdir().unwrap();
    let list = Connections::open(dir.path(), Arc::new(MemoryVault::default()), options()).unwrap();
    let added = list
        .add(NewConnection {
            name: "Missing".into(),
            key: None,
            server: Server::Local {
                command: "/nonexistent/tool-server".into(),
                args: vec![],
            },
            token: None,
        })
        .unwrap();
    until(&list, &added.id, State::Failed).await;
    let error = list.get(&added.id).unwrap().error.unwrap();
    assert!(error.contains("did not start"), "{error}");
    assert!(list.sign_in(&added.id).await.is_err());
    assert!(list
        .add(NewConnection {
            name: " ".into(),
            key: None,
            server: Server::Local {
                command: "x".into(),
                args: vec![],
            },
            token: None,
        })
        .is_err());
}
