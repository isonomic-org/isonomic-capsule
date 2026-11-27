// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The broker's `connections.*` calls, answered from [`Connections`].

use crate::{Connection, Connections, NewConnection, Server, State};
use broker::agents::BoxFuture;
use isonomic_proto::connections as wire;
use isonomic_proto::{Call, ErrorCode, Reply, WireError};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Plugs [`Connections`] into the broker.
pub struct Wire(pub Arc<Connections>);

fn to_wire(c: Connection) -> wire::Connection {
    wire::Connection {
        id: c.id,
        name: c.name,
        key: c.key,
        server: match c.server {
            Server::Remote { url } => wire::Server::Remote { url },
            Server::Local { command, args } => wire::Server::Local { command, args },
        },
        state: match c.state {
            State::NeedsSignIn => wire::ConnectionState::NeedsSignIn,
            State::Connecting => wire::ConnectionState::Connecting,
            State::Ready => wire::ConnectionState::Ready,
            State::Failed => wire::ConnectionState::Failed,
        },
        error: c.error,
        tools: c.tools,
    }
}

fn not_found(id: &str) -> WireError {
    WireError::new(
        ErrorCode::ConnectionNotFound,
        format!("There is no connection {id}."),
    )
}

impl Wire {
    fn known(&self, id: &str) -> Result<(), WireError> {
        self.0.get(id).map(|_| ()).ok_or_else(|| not_found(id))
    }
}

impl broker::Connections for Wire {
    fn call(&self, call: Call) -> BoxFuture<'_, Result<Reply, WireError>> {
        Box::pin(async move {
            match call {
                Call::ConnectionsList(_) => Ok(Reply::Connections(
                    self.0.list().into_iter().map(to_wire).collect(),
                )),
                Call::ConnectionsAdd(add) => {
                    let server = match add.server {
                        wire::Server::Remote { url } => Server::Remote { url },
                        wire::Server::Local { command, args } => Server::Local { command, args },
                        _ => {
                            return Err(WireError::new(
                                ErrorCode::BadRequest,
                                "This kind of tool server isn't supported.",
                            ))
                        }
                    };
                    let new = NewConnection {
                        name: add.name,
                        key: add.key,
                        server,
                        token: add.secret.map(|s| s.reveal().to_string()),
                    };
                    match self.0.add(new) {
                        Ok(c) => Ok(Reply::Connection(to_wire(c))),
                        Err(message) => Err(WireError::new(ErrorCode::BadRequest, message)),
                    }
                }
                Call::ConnectionsSignIn(sign_in) => {
                    self.known(&sign_in.id)?;
                    match self.0.sign_in(&sign_in.id).await {
                        Ok(url) => Ok(Reply::SignIn(wire::SignInStarted { url })),
                        Err(message) => Err(WireError::new(ErrorCode::SignInFailed, message)),
                    }
                }
                Call::ConnectionsRemove(remove) => {
                    self.known(&remove.id)?;
                    match self.0.remove(&remove.id).await {
                        Ok(()) => Ok(Reply::Done {}),
                        Err(message) => Err(WireError::new(ErrorCode::Internal, message)),
                    }
                }
                Call::ConnectionsTools(tools) => {
                    self.known(&tools.id)?;
                    match self.0.tools(&tools.id) {
                        Ok(list) => Ok(Reply::ConnectionTools(
                            list.into_iter()
                                .map(|t| wire::ConnectionTool {
                                    name: t.name,
                                    description: t.description,
                                })
                                .collect(),
                        )),
                        Err(message) => Err(WireError::new(ErrorCode::Internal, message)),
                    }
                }
                _ => Err(WireError::new(
                    ErrorCode::Unsupported,
                    "Not a connections call.",
                )),
            }
        })
    }

    fn changes(&self) -> Option<broadcast::Receiver<wire::Connection>> {
        let mut inner = self.0.subscribe();
        let (tx, rx) = broadcast::channel(64);
        tokio::spawn(async move {
            loop {
                match inner.recv().await {
                    Ok(c) => {
                        if tx.send(to_wire(c)).is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Some(rx)
    }
}

/// The search path a local tool server starts with: the system's, then
/// where command-line tools are commonly installed on a Mac.
pub const LOCAL_SERVER_PATH: &str =
    "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// The engine's connections: the list in `root`, secrets in the login
/// keychain on a Mac (in memory elsewhere, and for a scratch Capsule, so
/// they last one run), and local servers started with `child_env` and
/// nothing else.
pub fn open_for_engine(
    root: &std::path::Path,
    child_env: Vec<(String, String)>,
    scratch: bool,
) -> Result<Arc<Connections>, String> {
    #[cfg(target_os = "macos")]
    let vault: Arc<dyn crate::Vault> = match scratch {
        true => Arc::new(crate::MemoryVault::default()),
        false => Arc::new(crate::MacVault),
    };
    #[cfg(not(target_os = "macos"))]
    let vault: Arc<dyn crate::Vault> = {
        let _ = scratch;
        Arc::new(crate::MemoryVault::default())
    };
    let options = crate::Options {
        child_env,
        ..crate::Options::default()
    };
    Connections::open(root, vault, options)
}
