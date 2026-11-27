// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The seam to the person's tool server connections: the broker checks
//! that the caller is the shell (`proto::auth`), then asks here. The
//! connections themselves are kept by the `connections` crate, which
//! depends on this one.

use isonomic_proto::connections::Connection;
use isonomic_proto::{Call, ErrorCode, Reply, WireError};
use tokio::sync::broadcast;

use crate::agents::BoxFuture;

pub trait Connections: Send + Sync + 'static {
    /// Answer one `connections.*` call.
    fn call(&self, call: Call) -> BoxFuture<'_, Result<Reply, WireError>>;

    /// Each connection's changes, for the shell's status subscription.
    /// None by default.
    fn changes(&self) -> Option<broadcast::Receiver<Connection>> {
        None
    }
}

/// No connections service: every call is answered `unsupported`.
pub struct NoConnections;

impl Connections for NoConnections {
    fn call(&self, _call: Call) -> BoxFuture<'_, Result<Reply, WireError>> {
        Box::pin(async {
            Err(WireError::new(
                ErrorCode::Unsupported,
                "This engine has no tool server connections.",
            ))
        })
    }
}
