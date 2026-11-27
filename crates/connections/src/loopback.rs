// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The page the person's browser comes back to after a sign-in: a listener
//! on 127.0.0.1 at a port the system picks, owned by the engine for one
//! sign-in only. It takes one return to `/callback` whose `state` matches,
//! answers with a short page, and hands back the code.

use std::time::Duration;

use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// How long a sign-in may take in the browser.
pub const SIGN_IN_WAIT: Duration = Duration::from_secs(600);

pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

const DONE: &str = "<!doctype html><meta charset=utf-8><title>Signed in</title>\
    <body style=\"font-family:system-ui,sans-serif;padding:48px\">\
    <h2>You're signed in.</h2><p>You can close this window and go back to Isonomic.</p>";
const REFUSED: &str = "<!doctype html><meta charset=utf-8><title>Not signed in</title>\
    <body style=\"font-family:system-ui,sans-serif;padding:48px\">\
    <h2>The sign-in did not finish.</h2><p>Go back to Isonomic and try again.</p>";

impl Loopback {
    pub async fn bind() -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("The sign-in page could not start: {e}."))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("The sign-in page could not start: {e}."))?
            .port();
        Ok(Self { listener, port })
    }

    /// Where the sign-in server sends the browser back.
    pub fn redirect(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Wait for the browser's return with `state`, and give its code.
    pub async fn code(self, state: &str) -> Result<String, String> {
        let wait = async {
            loop {
                let (mut stream, _) = self.listener.accept().await.map_err(|e| e.to_string())?;
                let mut buf = vec![0u8; 8192];
                let n = match stream.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let head = String::from_utf8_lossy(&buf[..n]);
                let target = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                if !target.starts_with("/callback") {
                    let _ = stream
                        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    continue;
                }
                let url = Url::parse(&format!("http://127.0.0.1{target}"))
                    .map_err(|_| "The sign-in came back malformed.".to_string())?;
                let get = |k: &str| {
                    url.query_pairs()
                        .find(|(name, _)| name == k)
                        .map(|(_, v)| v.into_owned())
                };
                let outcome = match (get("state"), get("code"), get("error")) {
                    (Some(s), Some(code), None) if s == state => Ok(code),
                    (Some(s), _, _) if s != state => {
                        // Not this sign-in's return: keep waiting.
                        respond(&mut stream, REFUSED).await;
                        continue;
                    }
                    (_, _, Some(_)) => Err("The sign-in was turned down.".to_string()),
                    _ => Err("The sign-in came back without a code.".to_string()),
                };
                respond(&mut stream, if outcome.is_ok() { DONE } else { REFUSED }).await;
                return outcome;
            }
        };
        tokio::time::timeout(SIGN_IN_WAIT, wait)
            .await
            .map_err(|_| "The sign-in took longer than ten minutes.".to_string())?
    }
}

async fn respond(stream: &mut tokio::net::TcpStream, body: &str) {
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
}
