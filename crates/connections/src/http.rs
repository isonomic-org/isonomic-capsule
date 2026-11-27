// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A remote tool server over HTTP: each message is a POST, answered with
//! JSON or with an event stream that carries the response. The session id
//! the server gives on `initialize` (its session header) goes on every later
//! request, and so does a bearer token when the connection has one. A
//! refused token is refreshed once; a refusal without one asks for a
//! sign-in, carrying the metadata address the server named.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE,
};
use reqwest::StatusCode;
use serde_json::Value;

use crate::rpc::{id_of, is_response, BoxFuture, Failure, Transport};

const SESSION: &str = "mcp-session-id";
const VERSION: &str = "mcp-protocol-version";
/// The largest answer the engine reads from a server.
const MAX_BODY: usize = 8 << 20;

/// Where a remote connection's bearer token comes from.
pub trait Bearer: Send + Sync {
    /// The token to send now, if any.
    fn token(&self) -> Option<String>;
    /// Try to get a fresh token after a refusal; true if there is one.
    fn refresh(&self) -> BoxFuture<'_, bool>;
}

/// A token the person pasted: it never refreshes.
pub struct Pasted(pub String);

impl Bearer for Pasted {
    fn token(&self) -> Option<String> {
        Some(self.0.clone())
    }
    fn refresh(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
}

pub struct Http {
    client: reqwest::Client,
    url: String,
    bearer: Option<Arc<dyn Bearer>>,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
}

/// The HTTP client every remote connection uses: no redirects followed
/// (a token must not follow one to another host), bounded waits.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .expect("the HTTP client builds")
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The `resource_metadata` address in a `WWW-Authenticate` challenge.
pub fn resource_metadata(headers: &HeaderMap) -> Option<String> {
    let challenge = headers.get(WWW_AUTHENTICATE)?.to_str().ok()?;
    let at = challenge.find("resource_metadata=")? + "resource_metadata=".len();
    let rest = &challenge[at..];
    let value = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => rest.split([',', ' ']).next()?,
    };
    Some(value.to_string()).filter(|v| !v.is_empty())
}

fn unreachable(url: &str, e: reqwest::Error) -> Failure {
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string());
    let why = if e.is_timeout() {
        "it did not answer in time".to_string()
    } else {
        "it could not be reached".to_string()
    };
    Failure::failed(format!("The tool server at {host} is unavailable: {why}."))
}

impl Http {
    pub fn new(client: reqwest::Client, url: String, bearer: Option<Arc<dyn Bearer>>) -> Self {
        Self {
            client,
            url,
            bearer,
            session: Mutex::new(None),
            version: Mutex::new(None),
        }
    }

    async fn post(&self, message: &Value) -> Result<reqwest::Response, Failure> {
        let mut refreshed = false;
        loop {
            let mut request = self
                .client
                .post(&self.url)
                .header(ACCEPT, "application/json, text/event-stream")
                .header(CONTENT_TYPE, "application/json")
                .json(message);
            if let Some(session) = lock(&self.session).clone() {
                request = request.header(SESSION, session);
            }
            if let Some(version) = lock(&self.version).clone() {
                request = request.header(VERSION, version);
            }
            if let Some(token) = self.bearer.as_ref().and_then(|b| b.token()) {
                let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| Failure::failed("The connection's token is not valid."))?;
                value.set_sensitive(true);
                request = request.header(AUTHORIZATION, value);
            }
            let response = request
                .send()
                .await
                .map_err(|e| unreachable(&self.url, e))?;
            match response.status() {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    if !refreshed {
                        if let Some(bearer) = &self.bearer {
                            refreshed = true;
                            if bearer.refresh().await {
                                continue;
                            }
                        }
                    }
                    return Err(Failure::SignIn(resource_metadata(response.headers())));
                }
                StatusCode::NOT_FOUND if lock(&self.session).is_some() => {
                    // The server forgot the session: start over next time.
                    *lock(&self.session) = None;
                    return Err(Failure::failed("The tool server ended the session."));
                }
                status if !status.is_success() => {
                    return Err(Failure::failed(format!(
                        "The tool server answered {}.",
                        status.as_u16()
                    )));
                }
                _ => {}
            }
            if let Some(session) = response
                .headers()
                .get(SESSION)
                .and_then(|v| v.to_str().ok())
            {
                *lock(&self.session) = Some(session.to_string());
            }
            return Ok(response);
        }
    }
}

/// Find the response to `id` in an event stream's `data` lines.
async fn from_events(response: reqwest::Response, id: &str) -> Result<Value, Failure> {
    let mut body = response.bytes_stream();
    let (mut buffer, mut data, mut read) = (Vec::<u8>::new(), String::new(), 0usize);
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|_| Failure::failed("The tool server's answer broke off."))?;
        read += chunk.len();
        if read > MAX_BODY {
            return Err(Failure::failed("The tool server's answer is too large."));
        }
        buffer.extend_from_slice(&chunk);
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some(rest) = line.strip_prefix("data:") {
                data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
                data.push('\n');
            } else if line.is_empty() && !data.is_empty() {
                let event = std::mem::take(&mut data);
                if let Ok(message) = serde_json::from_str::<Value>(&event) {
                    if is_response(&message) && id_of(&message).as_deref() == Some(id) {
                        return Ok(message);
                    }
                }
            }
        }
    }
    Err(Failure::failed("The tool server ended without answering."))
}

/// Find the response to `id` in a JSON body: one message, or a batch.
fn from_json(body: &[u8], id: &str) -> Result<Value, Failure> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| Failure::failed("The tool server's answer is not JSON."))?;
    let messages = match value {
        Value::Array(items) => items,
        one => vec![one],
    };
    messages
        .into_iter()
        .find(|m| is_response(m) && id_of(m).as_deref() == Some(id))
        .ok_or_else(|| Failure::failed("The tool server answered something else."))
}

impl Transport for Http {
    fn request(&self, message: Value) -> BoxFuture<'_, Result<Value, Failure>> {
        Box::pin(async move {
            let id = id_of(&message).ok_or_else(|| Failure::failed("a request without an id"))?;
            let response = self.post(&message).await?;
            let events = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|t| t.starts_with("text/event-stream"));
            if events {
                return from_events(response, &id).await;
            }
            let body = response
                .bytes()
                .await
                .map_err(|_| Failure::failed("The tool server's answer broke off."))?;
            if body.len() > MAX_BODY {
                return Err(Failure::failed("The tool server's answer is too large."));
            }
            from_json(&body, &id)
        })
    }

    fn notify(&self, message: Value) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move { self.post(&message).await.map(|_| ()) })
    }

    fn agreed(&self, version: &str) {
        *lock(&self.version) = Some(version.to_string());
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let Some(session) = lock(&self.session).take() else {
                return;
            };
            let mut request = self.client.delete(&self.url).header(SESSION, session);
            if let Some(token) = self.bearer.as_ref().and_then(|b| b.token()) {
                request = request.bearer_auth(token);
            }
            let _ = request.send().await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_names_the_metadata_quoted_or_not() {
        let mut headers = HeaderMap::new();
        let quoted = r#"Bearer realm="x", resource_metadata="https://a.example/.well-known/oauth-protected-resource""#;
        headers.insert(WWW_AUTHENTICATE, HeaderValue::from_static(quoted));
        assert_eq!(
            resource_metadata(&headers).as_deref(),
            Some("https://a.example/.well-known/oauth-protected-resource")
        );
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer resource_metadata=https://b.example/m, scope=x"),
        );
        assert_eq!(
            resource_metadata(&headers).as_deref(),
            Some("https://b.example/m")
        );
        headers.insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        assert_eq!(resource_metadata(&headers), None);
    }

    #[test]
    fn a_batch_answers_by_id() {
        let body = br#"[{"jsonrpc":"2.0","method":"notifications/x"},{"jsonrpc":"2.0","id":3,"result":{"ok":true}}]"#;
        assert_eq!(from_json(body, "3").unwrap()["result"]["ok"], true);
        assert!(from_json(body, "4").is_err());
    }
}
