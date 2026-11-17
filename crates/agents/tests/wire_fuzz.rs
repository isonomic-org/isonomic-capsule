// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The engine's socket under hostile input: random bytes, frames that lie
//! about their length, oversized frames, frames cut short and held open,
//! deeply nested JSON, and every client request in the SDK's frozen fixtures
//! with its bytes mutated. The engine must never panic, and no connection's
//! bad input may stall another: a well-behaved client keeps getting answers
//! throughout.
//!
//! Hermetic: the test engine in `support`, a seeded generator, so every run
//! sends the same input. Real time appears only as a safety limit on waits.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use capsule_core::CapsulePaths;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// The longest any one exchange is waited for.
const LIMIT: Duration = Duration::from_secs(5);
/// How many mutated requests are sent.
const ROUNDS: usize = 400;

/// xorshift64*: small, seeded, the same sequence every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(body);
    out
}

/// Every client request line in the SDK's fixtures, but shutdown.
fn corpus() -> Vec<Vec<u8>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../isonomic-sdk/crates/proto/fixtures/v0");
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the SDK's fixtures") {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            if line.contains("\"type\":\"request\"") && !line.contains("system.shutdown") {
                lines.push(line.as_bytes().to_vec());
            }
        }
    }
    assert!(lines.len() > 20, "found {} requests", lines.len());
    lines
}

/// One of a few ways to spoil a message.
fn mutate(rng: &mut Rng, line: &[u8]) -> Vec<u8> {
    let mut out = line.to_vec();
    match rng.below(5) {
        // Flip a few bytes.
        0 => {
            for _ in 0..=rng.below(4) {
                let i = rng.below(out.len());
                out[i] = rng.next() as u8;
            }
        }
        // Cut it short.
        1 => out.truncate(rng.below(out.len())),
        // Splice in random bytes.
        2 => {
            let at = rng.below(out.len());
            let n = 1 + rng.below(32);
            let junk = rng.bytes(n);
            out.splice(at..at, junk);
        }
        // Swap a value for a huge number, a deep array or a long string.
        3 => {
            let swap: &[u8] = match rng.below(3) {
                0 => b"18446744073709551616000",
                1 => b"[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[",
                _ => b"\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"",
            };
            if let Some(at) = out.iter().position(|&b| b == b':') {
                out.splice(at + 1..(at + 2).min(out.len()), swap.iter().copied());
            }
        }
        // Leave it whole: the engine must still answer it.
        _ => {}
    }
    out
}

async fn hello(socket: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let hello = br#"{"type":"hello","versions":[0],"client":"fuzz"}"#;
    stream.write_all(&frame(hello)).await.unwrap();
    stream
}

/// Send `bytes`, then read until the engine answers, closes, or the limit
/// passes. Never fails on what the engine says; says whether an answer came.
async fn exchange(stream: &mut UnixStream, bytes: &[u8]) -> bool {
    if stream.write_all(bytes).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 4096];
    matches!(
        tokio::time::timeout(LIMIT, stream.read(&mut buf)).await,
        Ok(Ok(n)) if n > 0
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_input_never_panics_the_engine_or_stalls_another_client() {
    let panics = Arc::new(AtomicUsize::new(0));
    {
        let panics = Arc::clone(&panics);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            panics.fetch_add(1, Ordering::SeqCst);
            previous(info);
        }));
    }
    let dir = tempfile::tempdir().unwrap();
    let shell = support::shell(dir.path(), 9).await;
    let socket = CapsulePaths::new(dir.path()).engine_socket();
    let healthy = || async {
        tokio::time::timeout(LIMIT, shell.status())
            .await
            .expect("a well-behaved client is answered")
            .expect("status")
    };
    healthy().await;
    let mut rng = Rng(0x15_0e_c0_de);

    // Held open throughout: a frame that promises 100 bytes and sends 10,
    // and a connection that never says hello.
    let mut stalled = hello(&socket).await;
    stalled.write_all(&100u32.to_be_bytes()).await.unwrap();
    stalled.write_all(&[b'{'; 10]).await.unwrap();
    let _silent = UnixStream::connect(&socket).await.unwrap();
    healthy().await;

    // Garbage before any hello, and frames that lie.
    for _ in 0..50 {
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        let n = 1 + rng.below(200);
        let junk = rng.bytes(n);
        let _ =
            tokio::time::timeout(Duration::from_millis(100), exchange(&mut stream, &junk)).await;
    }
    for len in [0u32, 1, u32::MAX, (1 << 20) + 1] {
        let mut stream = hello(&socket).await;
        let _ = tokio::time::timeout(
            Duration::from_millis(100),
            exchange(&mut stream, &len.to_be_bytes()),
        )
        .await;
    }
    let mut deep = hello(&socket).await;
    let nested = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    let _ = exchange(&mut deep, &frame(nested.as_bytes())).await;
    healthy().await;

    // Every fixture request, spoiled, on a few connections at once.
    let corpus = corpus();
    let mut streams = Vec::new();
    for _ in 0..4 {
        streams.push(hello(&socket).await);
    }
    // The greetings are answered first.
    for stream in &mut streams {
        let mut buf = [0u8; 4096];
        let _ = tokio::time::timeout(LIMIT, stream.read(&mut buf)).await;
    }
    let mut answered = 0;
    for round in 0..ROUNDS {
        let line = &corpus[rng.below(corpus.len())];
        let body = mutate(&mut rng, line);
        let count = streams.len();
        let slot = round % count;
        if exchange(&mut streams[slot], &frame(&body)).await {
            answered += 1;
        } else {
            // A closed connection is replaced, so every round is heard.
            streams[slot] = hello(&socket).await;
        }
        if round % 50 == 0 {
            healthy().await;
        }
    }
    // A frame whose length is intact is always answered, whatever is in it.
    assert!(
        answered >= ROUNDS * 9 / 10,
        "{answered} of {ROUNDS} answered"
    );

    healthy().await;
    drop(stalled);
    assert_eq!(panics.load(Ordering::SeqCst), 0, "the engine panicked");
}
