// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Two-phase health probe: TCP connect (liveness) then HTTP GET (readiness).
//! Works over TCP or a Unix socket.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::endpoint::Endpoint;

/// TCP connect + `GET {path}`; healthy iff a 200 status line arrives within `timeout`.
pub async fn probe_http(endpoint: &Endpoint, path: &str, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, probe_inner(endpoint, path))
        .await
        .unwrap_or_default()
}

async fn probe_inner(endpoint: &Endpoint, path: &str) -> bool {
    let Ok(mut stream) = endpoint.connect().await else {
        return false;
    };
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        endpoint.host_header()
    );
    if stream.write_all(request.as_bytes()).await.is_err() {
        return false;
    }
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(2).any(|w| w == b"\r\n") {
                    break;
                }
            }
            Err(_) => return false,
        }
    }
    let head = String::from_utf8_lossy(&buf);
    head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(flavor = "multi_thread")]
    async fn probes_a_healthy_server_true_and_a_dead_port_false() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
        });
        assert!(probe_http(&Endpoint::Tcp(addr), "/health", Duration::from_secs(2)).await);

        // A port with nothing listening: allocate one, then close it.
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);
        assert!(
            !probe_http(
                &Endpoint::Tcp(dead_addr),
                "/health",
                Duration::from_millis(500)
            )
            .await
        );
    }
}
