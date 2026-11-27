// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A fake remote tool server and its sign-in server, in the test process
//! on a loopback port: the protocol over HTTP (JSON and event-stream
//! answers, a session id), the protected-resource and authorization-server
//! metadata, client registration, and a token endpoint that checks the
//! PKCE verifier. Tokens it accepts can be changed mid-test.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Default)]
pub struct State {
    /// Bearer tokens the tool server takes.
    pub accepted: Vec<String>,
    /// The challenge the sign-in page was opened with.
    pub challenge: Option<String>,
    /// Whether the sign-in server takes registrations.
    pub no_registration: bool,
    /// Session ids sent on requests after `initialize`.
    pub sessions_seen: Vec<String>,
    /// The arguments of each tool call.
    pub calls: Vec<Value>,
    pub refreshed: u32,
}

pub struct Fake {
    pub port: u16,
    pub state: Arc<Mutex<State>>,
}

impl Fake {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/tools", self.port)
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

pub async fn start() -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state: Arc<Mutex<State>> = Arc::default();
    let shared = Arc::clone(&state);
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let state = Arc::clone(&shared);
            tokio::spawn(serve(socket, port, state));
        }
    });
    Fake { port, state }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

async fn read(socket: &mut TcpStream) -> Option<Request> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let end = loop {
        if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        let n = socket.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    let mut body = data[end + 4..].to_vec();
    while body.len() < length {
        let n = socket.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

async fn reply(socket: &mut TcpStream, status: &str, headers: &[(&str, String)], body: &str) {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    let _ = socket.write_all(out.as_bytes()).await;
}

fn form(body: &[u8]) -> HashMap<String, String> {
    reqwest::Url::parse(&format!("http://x/?{}", String::from_utf8_lossy(body)))
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn b64(bytes: &[u8]) -> String {
    connections::oauth::base64url(bytes)
}

async fn serve(mut socket: TcpStream, port: u16, state: Arc<Mutex<State>>) {
    let Some(req) = read(&mut socket).await else {
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let json_type = ("Content-Type", "application/json".to_string());
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/.well-known/oauth-protected-resource/tools") => {
            let body = json!({"resource": format!("{base}/tools"),
                              "authorization_servers": [format!("{base}/auth")],
                              "scopes_supported": ["notes"]});
            reply(&mut socket, "200 OK", &[json_type], &body.to_string()).await;
        }
        ("GET", "/.well-known/oauth-authorization-server/auth") => {
            let mut body = json!({
                "issuer": format!("{base}/auth"),
                "authorization_endpoint": format!("{base}/auth/authorize"),
                "token_endpoint": format!("{base}/auth/token"),
                "registration_endpoint": format!("{base}/auth/register"),
                "code_challenge_methods_supported": ["S256"],
            });
            if state.lock().unwrap().no_registration {
                body.as_object_mut()
                    .unwrap()
                    .remove("registration_endpoint");
            }
            reply(&mut socket, "200 OK", &[json_type], &body.to_string()).await;
        }
        ("POST", "/auth/register") => {
            let asked: Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(asked["token_endpoint_auth_method"], "none");
            let body = json!({"client_id": "client-1"});
            reply(&mut socket, "201 Created", &[json_type], &body.to_string()).await;
        }
        ("POST", "/auth/token") => {
            let f = form(&req.body);
            let body = {
                let mut s = state.lock().unwrap();
                match f.get("grant_type").map(String::as_str) {
                    Some("authorization_code") => {
                        let verifier = f.get("code_verifier").cloned().unwrap_or_default();
                        let ok = f.get("code").map(String::as_str) == Some("code-1")
                            && f.get("client_id").map(String::as_str) == Some("client-1")
                            && s.challenge.as_deref()
                                == Some(&b64(&Sha256::digest(verifier.as_bytes())));
                        ok.then(|| {
                            json!({"access_token": "tok-1", "refresh_token": "ref-1",
                                      "expires_in": 3600, "token_type": "Bearer"})
                        })
                    }
                    Some("refresh_token")
                        if f.get("refresh_token").map(String::as_str) == Some("ref-1") =>
                    {
                        s.refreshed += 1;
                        Some(json!({"access_token": "tok-2", "token_type": "Bearer"}))
                    }
                    _ => None,
                }
            };
            match body {
                Some(b) => reply(&mut socket, "200 OK", &[json_type], &b.to_string()).await,
                None => reply(&mut socket, "400 Bad Request", &[], "").await,
            }
        }
        ("POST", "/tools") => tool_server(&mut socket, &base, req, &state).await,
        ("DELETE", "/tools") => reply(&mut socket, "200 OK", &[], "").await,
        _ => reply(&mut socket, "404 Not Found", &[], "").await,
    }
}

async fn tool_server(socket: &mut TcpStream, base: &str, req: Request, state: &Mutex<State>) {
    let token = req
        .headers
        .get("authorization")
        .and_then(|a| a.strip_prefix("Bearer "))
        .map(str::to_string);
    let allowed = token.is_some_and(|t| state.lock().unwrap().accepted.contains(&t));
    if !allowed {
        let challenge = format!(
            "Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/tools\""
        );
        reply(
            socket,
            "401 Unauthorized",
            &[("WWW-Authenticate", challenge)],
            "",
        )
        .await;
        return;
    }
    let message: Value = serde_json::from_slice(&req.body).unwrap();
    let method = message["method"].as_str().unwrap_or_default().to_string();
    if method != "initialize" {
        let session = req
            .headers
            .get("mcp-session-id")
            .cloned()
            .unwrap_or_default();
        state.lock().unwrap().sessions_seen.push(session);
    }
    let id = message["id"].clone();
    let events = ("Content-Type", "text/event-stream".to_string());
    match method.as_str() {
        "initialize" => {
            let body = json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake", "version": "1"}}});
            let headers = [
                ("Content-Type", "application/json".to_string()),
                ("Mcp-Session-Id", "session-1".to_string()),
            ];
            reply(socket, "200 OK", &headers, &body.to_string()).await;
        }
        "notifications/initialized" => reply(socket, "202 Accepted", &[], "").await,
        "tools/list" => {
            let body = json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [
                {"name": "search", "description": "Search notes.",
                 "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}},
                 "annotations": {"readOnlyHint": true}},
                {"name": "delete", "description": "Delete a note."}]}});
            let stream = format!(": opened\n\nevent: message\ndata: {body}\n\n");
            reply(socket, "200 OK", &[events], &stream).await;
        }
        "tools/call" => {
            let arguments = message["params"]["arguments"].clone();
            state.lock().unwrap().calls.push(arguments.clone());
            let progress =
                json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}});
            let body = json!({"jsonrpc": "2.0", "id": id, "result": {
                "content": [{"type": "text", "text": format!("found {}", arguments["q"])}]}});
            let stream = format!("data: {progress}\n\ndata: {body}\n\n");
            reply(socket, "200 OK", &[events], &stream).await;
        }
        _ => reply(socket, "400 Bad Request", &[], "").await,
    }
}
