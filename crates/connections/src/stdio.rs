// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A local tool server: a command the engine starts, spoken to with one
//! JSON-RPC message a line on its standard input and output. The child gets
//! the allow-listed environment it is given and nothing of the engine's
//! own. Its standard error is dropped: a server may print secrets there.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

use crate::rpc::{answer_server, id_of, is_response, BoxFuture, Failure, Transport};

/// How long a request waits for its answer.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

pub struct StdioServer {
    child: tokio::sync::Mutex<Option<Child>>,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    pending: Pending,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl StdioServer {
    /// Start `command` with `args`, in an environment of `env` only.
    pub fn start(
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<Self, Failure> {
        let mut child = Command::new(command)
            .args(args)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                Failure::failed(format!("The tool server {command} did not start: {e}"))
            })?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stdin = Arc::new(tokio::sync::Mutex::new(stdin));
        let pending: Pending = Arc::default();
        let (reader_pending, reader_stdin) = (Arc::clone(&pending), Arc::clone(&stdin));
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if is_response(&message) {
                    let waiter = id_of(&message).and_then(|id| lock(&reader_pending).remove(&id));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(message);
                    }
                } else if let Some(answer) = answer_server(&message) {
                    let _ = write_line(&reader_stdin, &answer).await;
                }
            }
            // The server is gone: every waiter learns so.
            lock(&reader_pending).clear();
        });
        Ok(Self {
            child: tokio::sync::Mutex::new(Some(child)),
            stdin,
            pending,
        })
    }
}

async fn write_line(
    stdin: &tokio::sync::Mutex<ChildStdin>,
    message: &Value,
) -> Result<(), Failure> {
    let mut line = serde_json::to_vec(message).map_err(|e| Failure::failed(e.to_string()))?;
    line.push(b'\n');
    let mut stdin = stdin.lock().await;
    stdin
        .write_all(&line)
        .await
        .map_err(|_| Failure::failed("The tool server stopped."))?;
    stdin
        .flush()
        .await
        .map_err(|_| Failure::failed("The tool server stopped."))
}

impl Transport for StdioServer {
    fn request(&self, message: Value) -> BoxFuture<'_, Result<Value, Failure>> {
        Box::pin(async move {
            let id = id_of(&message).ok_or_else(|| Failure::failed("a request without an id"))?;
            let (tx, rx) = oneshot::channel();
            lock(&self.pending).insert(id.clone(), tx);
            if let Err(e) = write_line(&self.stdin, &message).await {
                lock(&self.pending).remove(&id);
                return Err(e);
            }
            match tokio::time::timeout(ANSWER_TIMEOUT, rx).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(_)) => Err(Failure::failed("The tool server stopped.")),
                Err(_) => {
                    lock(&self.pending).remove(&id);
                    Err(Failure::failed("The tool server did not answer in time."))
                }
            }
        })
    }

    fn notify(&self, message: Value) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move { write_line(&self.stdin, &message).await })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(mut child) = self.child.lock().await.take() {
                let _ = child.kill().await;
            }
        })
    }
}
