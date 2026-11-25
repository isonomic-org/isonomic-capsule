// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A model file's fetch through what a first download meets: a quit, a
//! dropped connection, a silent one, a server that ignores ranges, a
//! corrupt leftover. The file comes from a loopback server in the test;
//! the stall wait is ended on demand, so no test waits on a timer.

use std::path::Path;
use std::sync::{Arc, Mutex};

use gateway::fetch::{fetch, part_path, FetchError, Stall};
use isonomic_model_library::ModelSpec;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

const LEN: usize = 200_000;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Honour ranges and send everything.
    Whole,
    /// Answer every request with the whole file, ranges or not.
    IgnoreRange,
    /// Send the first half of the file, then close the connection.
    CutAtHalf,
    /// Send the first half of the file, then go silent.
    SilentAtHalf,
}

struct Server {
    port: u16,
    mode: Arc<Mutex<Mode>>,
    /// The `Range` header of each request, if any.
    ranges: Arc<Mutex<Vec<Option<String>>>>,
}

fn body() -> Vec<u8> {
    (0..LEN).map(|i| (i % 251) as u8).collect()
}

fn spec(port: u16) -> ModelSpec {
    ModelSpec {
        file_name: "model.gguf".into(),
        url: format!("http://127.0.0.1:{port}/model.gguf"),
        sha256_hex: Sha256::digest(body())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        size_bytes: LEN as u64,
    }
}

async fn server(mode: Mode) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mode = Arc::new(Mutex::new(mode));
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let (m, r) = (Arc::clone(&mode), Arc::clone(&ranges));
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mode = *m.lock().unwrap();
            let ranges = Arc::clone(&r);
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&request).to_lowercase();
                let range = text
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .map(|r| r.trim_end_matches('-').trim().to_string());
                ranges.lock().unwrap().push(range.clone());
                let start = match (mode, &range) {
                    (Mode::IgnoreRange, _) | (_, None) => 0,
                    (_, Some(r)) => r.parse::<usize>().unwrap(),
                };
                let body = body();
                let head = if start == 0 {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {LEN}\r\n\r\n")
                } else {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\
                         Content-Range: bytes {start}-{}/{LEN}\r\n\r\n",
                        LEN - start,
                        LEN - 1
                    )
                };
                let _ = socket.write_all(head.as_bytes()).await;
                let end = match mode {
                    Mode::CutAtHalf | Mode::SilentAtHalf => LEN / 2,
                    _ => LEN,
                };
                let _ = socket.write_all(&body[start..end.max(start)]).await;
                let _ = socket.flush().await;
                if mode == Mode::SilentAtHalf {
                    std::future::pending::<()>().await;
                }
            });
        }
    });
    Server { port, mode, ranges }
}

/// A stall wait that ends only when the test says so.
fn stall_on(notify: &Arc<Notify>) -> Stall {
    let notify = Arc::clone(notify);
    Arc::new(move || {
        let notify = Arc::clone(&notify);
        Box::pin(async move { notify.notified().await })
    })
}

fn never() -> Stall {
    Arc::new(|| Box::pin(std::future::pending()))
}

async fn get(server: &Server, dir: &Path, stall: &Stall) -> Result<std::path::PathBuf, FetchError> {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut seen = |_: u64, _: Option<u64>| {};
    fetch(&client, &spec(server.port), dir, stall, &mut seen).await
}

fn installed(dir: &Path) -> bool {
    std::fs::read(dir.join("model.gguf")).is_ok_and(|b| b == body())
}

fn part(dir: &Path, port: u16) -> Option<usize> {
    std::fs::metadata(part_path(dir, &spec(port)))
        .ok()
        .map(|m| m.len() as usize)
}

#[tokio::test]
async fn a_whole_fetch_installs_and_leaves_no_part() {
    let server = server(Mode::Whole).await;
    let dir = tempfile::tempdir().unwrap();
    get(&server, dir.path(), &never()).await.unwrap();
    assert!(installed(dir.path()));
    assert_eq!(part(dir.path(), server.port), None);
}

#[tokio::test]
async fn a_dropped_connection_keeps_its_part_and_the_next_fetch_continues_it() {
    let server = server(Mode::CutAtHalf).await;
    let dir = tempfile::tempdir().unwrap();
    let cut = get(&server, dir.path(), &never()).await;
    assert!(cut.is_err(), "{cut:?}");
    assert!(!installed(dir.path()), "a half file counted as installed");
    assert_eq!(part(dir.path(), server.port), Some(LEN / 2));

    *server.mode.lock().unwrap() = Mode::Whole;
    get(&server, dir.path(), &never()).await.unwrap();
    assert!(installed(dir.path()));
    let ranges = server.ranges.lock().unwrap().clone();
    assert_eq!(ranges, [None, Some((LEN / 2).to_string())]);
}

#[tokio::test]
async fn a_silent_connection_fails_as_stalled_and_resumes_later() {
    let server = server(Mode::SilentAtHalf).await;
    let dir = tempfile::tempdir().unwrap();
    let notify = Arc::new(Notify::new());
    let stall = stall_on(&notify);
    let path = dir.path().to_path_buf();
    let port = server.port;
    let silent = tokio::spawn(async move {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut seen = |_: u64, _: Option<u64>| {};
        fetch(&client, &spec(port), &path, &stall, &mut seen).await
    });
    // Half the file is on disk once it has gone silent.
    while part(dir.path(), port) != Some(LEN / 2) {
        tokio::task::yield_now().await;
    }
    notify.notify_one();
    let outcome = silent.await.unwrap();
    assert!(matches!(outcome, Err(FetchError::Stalled)), "{outcome:?}");
    assert!(!installed(dir.path()));

    *server.mode.lock().unwrap() = Mode::Whole;
    get(&server, dir.path(), &never()).await.unwrap();
    assert!(installed(dir.path()));
}

#[tokio::test]
async fn a_server_that_ignores_the_range_gets_a_clean_restart() {
    let server = server(Mode::IgnoreRange).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(part_path(dir.path(), &spec(server.port)), &body()[..1000]).unwrap();
    get(&server, dir.path(), &never()).await.unwrap();
    assert!(installed(dir.path()));
    assert_eq!(server.ranges.lock().unwrap().clone(), [Some("1000".into())]);
}

#[tokio::test]
async fn a_corrupt_leftover_is_thrown_away_and_never_installed() {
    let server = server(Mode::Whole).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(part_path(dir.path(), &spec(server.port)), vec![9u8; 1000]).unwrap();
    let bad = get(&server, dir.path(), &never()).await;
    assert!(matches!(bad, Err(FetchError::Checksum)), "{bad:?}");
    assert!(!dir.path().join("model.gguf").exists());
    assert_eq!(part(dir.path(), server.port), None);
    get(&server, dir.path(), &never()).await.unwrap();
    assert!(installed(dir.path()));
}

#[tokio::test]
async fn a_whole_part_left_before_its_rename_installs_without_a_request() {
    let server = server(Mode::Whole).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(part_path(dir.path(), &spec(server.port)), body()).unwrap();
    get(&server, dir.path(), &never()).await.unwrap();
    assert!(installed(dir.path()));
    assert!(server.ranges.lock().unwrap().is_empty());
}
