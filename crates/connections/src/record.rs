// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the engine keeps about a connection, and what it says about one.

use serde::{Deserialize, Serialize};

/// Where a tool server runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Server {
    /// Reached over the network, by HTTP.
    Remote { url: String },
    /// Started by the engine on this computer, spoken to over its
    /// standard input and output.
    Local {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

/// One connection as kept in the connections file. No secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub server: Server,
}

/// Where a connection stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    NeedsSignIn,
    Connecting,
    Ready,
    Failed,
}

/// A connection as the shell sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub key: Option<String>,
    pub server: Server,
    pub state: State,
    /// With `Failed`: plain words to show as given.
    pub error: Option<String>,
    pub tools: u32,
}

/// One tool a connection offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    /// The server says the tool only reads (its `readOnlyHint`); without
    /// that, a tool is taken to act.
    pub read_only: bool,
    /// The tool's input schema, as JSON.
    pub input_schema: serde_json::Value,
}

/// A new connection.
#[derive(Clone)]
pub struct NewConnection {
    pub name: String,
    pub key: Option<String>,
    pub server: Server,
    /// A pasted token, for a remote server that takes one instead of a
    /// sign-in. Kept in the vault, never in the file.
    pub token: Option<String>,
}

impl std::fmt::Debug for NewConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewConnection")
            .field("name", &self.name)
            .field("key", &self.key)
            .field("server", &self.server)
            .field("token", &self.token.as_ref().map(|_| "[secret]"))
            .finish()
    }
}
