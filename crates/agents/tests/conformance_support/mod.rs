// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A stand-in for the engine's tool server connections, for the
//! conformance run only: it keeps connections in memory and reaches no
//! server, so the checks exercise the broker's routing, its shell-only
//! rule and the wire shapes. The real connections replace it here once
//! they land.

use std::sync::Mutex;

use broker::agents::BoxFuture;
use isonomic_proto::connections::{Connection, ConnectionState, Server};
use isonomic_proto::{Call, ErrorCode, Reply, WireError};

#[derive(Default)]
pub struct MemoryConnections {
    list: Mutex<(u64, Vec<Connection>)>,
}

fn not_found(id: &str) -> WireError {
    WireError::new(ErrorCode::ConnectionNotFound, format!("no connection {id}"))
}

impl MemoryConnections {
    fn answer(&self, call: Call) -> Result<Reply, WireError> {
        let mut guard = self.list.lock().unwrap();
        let (next, list) = &mut *guard;
        let find = |list: &[Connection], id: &str| list.iter().position(|c| c.id == id);
        match call {
            Call::ConnectionsList(_) => Ok(Reply::Connections(list.clone())),
            Call::ConnectionsAdd(add) => {
                if add.name.trim().is_empty() {
                    return Err(WireError::new(ErrorCode::BadRequest, "a blank name"));
                }
                *next += 1;
                let state = match (&add.server, &add.secret) {
                    (Server::Remote { .. }, None) => ConnectionState::NeedsSignIn,
                    _ => ConnectionState::Ready,
                };
                let connection = Connection {
                    id: format!("conn-{next}"),
                    name: add.name,
                    key: add.key,
                    server: add.server,
                    state,
                    error: None,
                    tools: 0,
                };
                list.push(connection.clone());
                Ok(Reply::Connection(connection))
            }
            Call::ConnectionsSignIn(s) => {
                find(list, &s.id).ok_or_else(|| not_found(&s.id))?;
                let message = "this stand-in signs in to nothing";
                Err(WireError::new(ErrorCode::SignInFailed, message))
            }
            Call::ConnectionsRemove(r) => {
                let at = find(list, &r.id).ok_or_else(|| not_found(&r.id))?;
                list.remove(at);
                Ok(Reply::Done {})
            }
            Call::ConnectionsTools(t) => {
                find(list, &t.id).ok_or_else(|| not_found(&t.id))?;
                Ok(Reply::ConnectionTools(Vec::new()))
            }
            other => Err(WireError::new(
                ErrorCode::UnknownMethod,
                format!("{} is not a connections method", other.method()),
            )),
        }
    }
}

impl broker::Connections for MemoryConnections {
    fn call(&self, call: Call) -> BoxFuture<'_, Result<Reply, WireError>> {
        let answer = self.answer(call);
        Box::pin(async move { answer })
    }
}
