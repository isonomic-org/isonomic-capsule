// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tool server protocol's client side, over any [`Transport`]: JSON-RPC
//! 2.0 requests for `initialize`, `tools/list` and `tools/call`, and the
//! `notifications/initialized` notification.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use crate::record::ToolInfo;

/// The protocol revision the engine speaks; a server may answer with an
/// older one it supports, which the engine accepts.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// At most this many pages of `tools/list`, so a server cannot keep the
/// engine listing forever.
const MAX_PAGES: usize = 20;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Why a request did not get an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The server wants a sign-in. Carries the protected-resource metadata
    /// address its refusal named, if it named one.
    SignIn(Option<String>),
    /// Anything else, in plain words to show the person.
    Failed(String),
}

impl Failure {
    pub fn failed(message: impl Into<String>) -> Self {
        Failure::Failed(message.into())
    }
}

/// One way to reach a server: send a request and wait for its response,
/// or send a notification.
pub trait Transport: Send + Sync {
    /// Send `message` (a request with an `id`) and return the response
    /// with that id.
    fn request(&self, message: Value) -> BoxFuture<'_, Result<Value, Failure>>;
    /// Send `message` (a notification), with no answer.
    fn notify(&self, message: Value) -> BoxFuture<'_, Result<(), Failure>>;
    /// Learn the protocol revision the server agreed to, for the headers
    /// that need it.
    fn agreed(&self, _version: &str) {}
    /// Let go of the server: end its process, or its session.
    fn close(&self) -> BoxFuture<'_, ()>;
}

/// What a tool call gave back.
#[derive(Debug, Clone, PartialEq)]
pub struct CallOutcome {
    /// Its text content, joined; other content is named, not included.
    pub text: String,
    /// Its structured content, when it gave any.
    pub structured: Option<Value>,
    /// The tool said it failed (`isError`).
    pub is_error: bool,
}

/// A session with one server.
pub struct Session {
    transport: Box<dyn Transport>,
    next: AtomicU64,
}

impl Session {
    /// Open a session over `transport`: `initialize`, then
    /// `notifications/initialized`.
    pub async fn open(transport: Box<dyn Transport>) -> Result<Self, Failure> {
        let session = Self {
            transport,
            next: AtomicU64::new(1),
        };
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "isonomic-capsule", "version": env!("CARGO_PKG_VERSION")},
        });
        let result = session.request("initialize", params).await?;
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(PROTOCOL_VERSION)
            .to_string();
        session.transport.agreed(&version);
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        session.transport.notify(note).await?;
        Ok(session)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, Failure> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let response = self.transport.request(message).await?;
        result(response, method)
    }

    /// Every tool the server offers, following `nextCursor`.
    pub async fn tools(&self) -> Result<Vec<ToolInfo>, Failure> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let page = self.request("tools/list", params).await?;
            for tool in page
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(info) = tool_info(tool) {
                    tools.push(info);
                }
            }
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    /// Call `name` with `arguments` (a JSON object).
    pub async fn call(&self, name: &str, arguments: Value) -> Result<CallOutcome, Failure> {
        let params = json!({"name": name, "arguments": arguments});
        let result = self.request("tools/call", params).await?;
        Ok(outcome(&result))
    }

    pub async fn close(&self) {
        self.transport.close().await;
    }
}

/// A response's `result`, or its `error` in plain words.
fn result(response: Value, method: &str) -> Result<Value, Failure> {
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no reason given");
        return Err(Failure::failed(format!(
            "The tool server refused {method}: {message}"
        )));
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| Failure::failed(format!("The tool server's answer to {method} was empty.")))
}

/// One entry of `tools/list`. A tool is read-only only when the server
/// says so (`annotations.readOnlyHint`); otherwise it acts.
fn tool_info(tool: &Value) -> Option<ToolInfo> {
    let name = tool.get("name")?.as_str()?.to_string();
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let read_only = tool
        .pointer("/annotations/readOnlyHint")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let input_schema = tool
        .get("inputSchema")
        .filter(|s| s.is_object())
        .cloned()
        .unwrap_or_else(|| json!({"type": "object"}));
    Some(ToolInfo {
        name,
        description,
        read_only,
        input_schema,
    })
}

fn outcome(result: &Value) -> CallOutcome {
    let mut text = String::new();
    for item in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let piece = match item.get("type").and_then(Value::as_str) {
            Some("text") => item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Some("resource") => item
                .pointer("/resource/text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| "[a resource]".into()),
            Some(kind) => format!("[{kind} content]"),
            None => continue,
        };
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&piece);
    }
    CallOutcome {
        text,
        structured: result.get("structuredContent").cloned(),
        is_error: result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// The id of a JSON-RPC message, as a key.
pub(crate) fn id_of(message: &Value) -> Option<String> {
    match message.get("id")? {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(format!("\"{s}\"")),
        _ => None,
    }
}

/// Whether `message` is a response (it has `result` or `error`).
pub(crate) fn is_response(message: &Value) -> bool {
    message.get("method").is_none()
        && (message.get("result").is_some() || message.get("error").is_some())
}

/// The answer to a request the server sent the engine: `ping` gets an
/// empty result, anything else "method not found".
pub(crate) fn answer_server(message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message.get("method")?.as_str()?;
    Some(match method {
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        _ => json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": "method not found"}}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_acts_unless_the_server_says_it_only_reads() {
        let read = json!({"name": "find", "annotations": {"readOnlyHint": true}});
        let act = json!({"name": "send", "description": "Send it.",
                         "inputSchema": {"type": "object", "properties": {}}});
        assert!(tool_info(&read).unwrap().read_only);
        let act = tool_info(&act).unwrap();
        assert!(!act.read_only);
        assert_eq!(act.description, "Send it.");
        assert!(tool_info(&json!({"description": "no name"})).is_none());
    }

    #[test]
    fn a_calls_text_is_joined_and_other_content_named() {
        let result = json!({"content": [
            {"type": "text", "text": "one"},
            {"type": "image", "data": "...", "mimeType": "image/png"},
            {"type": "text", "text": "two"}
        ], "isError": true});
        let out = outcome(&result);
        assert_eq!(out.text, "one\n[image content]\ntwo");
        assert!(out.is_error);
    }

    #[test]
    fn an_error_response_is_said_in_plain_words() {
        let e = result(
            json!({"error": {"code": -1, "message": "no"}}),
            "tools/call",
        );
        assert_eq!(
            e.unwrap_err(),
            Failure::failed("The tool server refused tools/call: no")
        );
    }

    #[test]
    fn servers_requests_are_answered() {
        let ping = answer_server(&json!({"jsonrpc": "2.0", "id": 7, "method": "ping"})).unwrap();
        assert_eq!(ping["result"], json!({}));
        let other = answer_server(&json!({"id": "x", "method": "sampling/createMessage"}));
        assert_eq!(other.unwrap()["error"]["code"], -32601);
        assert!(answer_server(&json!({"method": "notifications/progress"})).is_none());
    }
}
