// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The harness's connected-apps service, over the person's connections.
//!
//! A handle is a connection's id and its operations are the tools it
//! offers. Every app sees every ready connection until app manifests
//! declare the connections they need; secrets never pass through here.
//! A result is cut to [`MAX_RESULT_BYTES`], saying so, so one tool cannot
//! fill the model's context.

use crate::Connections;
use harness_core::future::BoxFuture;
use harness_core::{AppId, Value};
use harness_toolkit::services::{self, ServiceError};
use std::sync::Arc;

/// The most a tool's result gives the model.
pub const MAX_RESULT_BYTES: usize = 16 * 1024;

/// `text` cut to [`MAX_RESULT_BYTES`] at a character boundary.
pub fn capped(mut text: String) -> String {
    if text.len() <= MAX_RESULT_BYTES {
        return text;
    }
    let mut end = MAX_RESULT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("\n[The result was cut here: it was too long.]");
    text
}

/// Plugs [`Connections`] into the harness.
pub struct Service(pub Arc<Connections>);

fn error(code: &str, message: impl Into<String>) -> ServiceError {
    ServiceError {
        code: code.into(),
        message: message.into(),
    }
}

impl services::Connections for Service {
    fn list(&self, _owner: &AppId) -> BoxFuture<'_, Vec<services::Connection>> {
        let ready = self
            .0
            .ready()
            .into_iter()
            .map(|(c, tools)| services::Connection {
                handle: c.id,
                label: c.name,
                operations: tools.into_iter().map(|t| t.name).collect(),
            })
            .collect();
        Box::pin(async move { ready })
    }

    fn call(
        &self,
        _owner: &AppId,
        handle: &str,
        operation: &str,
        input: Value,
    ) -> BoxFuture<'_, Result<Value, ServiceError>> {
        let (handle, operation) = (handle.to_string(), operation.to_string());
        Box::pin(async move {
            let arguments = match serde_json::to_value(&input) {
                Ok(serde_json::Value::Null) => serde_json::Value::Object(Default::default()),
                Ok(v) => v,
                Err(_) => return Err(error("connection.bad_input", "The input is not valid.")),
            };
            match self.0.call(&handle, &operation, arguments).await {
                Ok(out) if out.is_error => Err(error("connection.tool_error", capped(out.text))),
                Ok(out) => {
                    // Structured content, when a tool gives only that.
                    let text = match (out.text.is_empty(), out.structured) {
                        (true, Some(data)) => data.to_string(),
                        _ => out.text,
                    };
                    Ok(Value::from(capped(text)))
                }
                Err(message) => Err(error("connection.denied", message)),
            }
        })
    }
}
