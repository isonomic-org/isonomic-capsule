// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! After a stop, the next reply waits on the model server's own slot list
//! until no slot is working, then goes; it never waits past the limit.
//! A local stand-in answers `GET /slots`, checking the launch's key.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gateway::slots::{wait_idle, SlotWait, LIMIT};
use isonomic_llama::Endpoint;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A server whose slot 0 is working for the first `busy` asks.
async fn server(busy: usize) -> (Endpoint, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&asked);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let count = Arc::clone(&count);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let body = if !head.contains("authorization: bearer launch-key") {
                    return_status(&mut sock, "401 Unauthorized", "{}").await;
                    return;
                } else {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    let working = n < busy;
                    format!(
                        r#"[{{"id":0,"is_processing":{working}}},{{"id":1,"is_processing":false}}]"#
                    )
                };
                return_status(&mut sock, "200 OK", &body).await;
            });
        }
    });
    let endpoint = Endpoint {
        addr,
        api_key: Some("launch-key".into()),
    };
    (endpoint, asked)
}

async fn return_status(sock: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let reply = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(reply.as_bytes()).await;
}

#[tokio::test]
async fn the_reply_waits_until_the_stopped_task_has_ended() {
    let (endpoint, asked) = server(3).await;
    match wait_idle(&endpoint, LIMIT).await {
        SlotWait::Idle(waited) => assert!(waited < LIMIT, "{waited:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        asked.load(Ordering::SeqCst),
        4,
        "asked until the slot was idle"
    );
}

#[tokio::test]
async fn a_slot_that_stays_busy_is_waited_for_only_up_to_the_limit() {
    let (endpoint, _) = server(usize::MAX).await;
    let limit = Duration::from_millis(150);
    let started = std::time::Instant::now();
    assert_eq!(wait_idle(&endpoint, limit).await, SlotWait::StillBusy);
    assert!(started.elapsed() < limit + Duration::from_millis(200));
}

#[tokio::test]
async fn a_server_with_no_slot_list_is_said_so() {
    let (mut endpoint, _) = server(0).await;
    endpoint.api_key = Some("wrong".into());
    assert!(matches!(
        wait_idle(&endpoint, LIMIT).await,
        SlotWait::Unavailable(_)
    ));
}
