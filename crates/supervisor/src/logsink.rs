// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Log capture for supervised services: an in-memory ring of the last 50 lines
//! (what the window shows for a failing service) plus an optional
//! size-rotated log file.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Ring capacity (spec: last 50 log lines kept for the window).
pub const RING_CAPACITY: usize = 50;

/// Default rotation threshold for on-disk logs.
pub const DEFAULT_ROTATE_BYTES: u64 = 1024 * 1024;

pub struct LogSink {
    inner: Mutex<Inner>,
}

struct Inner {
    ring: VecDeque<String>,
    file: Option<File>,
    path: Option<PathBuf>,
    written: u64,
    rotate_bytes: u64,
}

impl LogSink {
    /// Ring buffer only (tests, ephemeral services).
    pub fn memory_only() -> Self {
        Self {
            inner: Mutex::new(Inner {
                ring: VecDeque::with_capacity(RING_CAPACITY),
                file: None,
                path: None,
                written: 0,
                rotate_bytes: DEFAULT_ROTATE_BYTES,
            }),
        }
    }

    /// Ring buffer + append-to-file with rotation at `rotate_bytes`
    /// (`file.log` → `file.log.1`, one generation kept).
    pub fn with_file(path: PathBuf, rotate_bytes: u64) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            inner: Mutex::new(Inner {
                ring: VecDeque::with_capacity(RING_CAPACITY),
                file: Some(file),
                path: Some(path),
                written,
                rotate_bytes,
            }),
        })
    }

    pub fn append(&self, line: &str) {
        let mut guard = self.inner.lock().expect("logsink lock");
        let inner = &mut *guard;
        if inner.ring.len() == RING_CAPACITY {
            inner.ring.pop_front();
        }
        inner.ring.push_back(line.to_string());

        if inner.file.is_some() {
            let bytes = line.len() as u64 + 1;
            if inner.written + bytes > inner.rotate_bytes {
                rotate(inner);
            }
            if let Some(file) = inner.file.as_mut() {
                let _ = writeln!(file, "{line}");
            }
            inner.written += bytes;
        }
    }

    /// Last ≤50 lines for the dashboard.
    pub fn last_lines(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("logsink lock")
            .ring
            .iter()
            .cloned()
            .collect()
    }
}

fn rotate(inner: &mut Inner) {
    let Some(path) = inner.path.clone() else {
        return;
    };
    inner.file = None; // close before rename
    let mut rotated = path.clone().into_os_string();
    rotated.push(".1");
    let _ = std::fs::rename(&path, std::path::Path::new(&rotated));
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => {
            inner.file = Some(file);
            inner.written = 0;
        }
        Err(_) => {
            inner.file = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_last_50_lines() {
        let sink = LogSink::memory_only();
        for i in 0..60 {
            sink.append(&format!("line {i}"));
        }
        let lines = sink.last_lines();
        assert_eq!(lines.len(), RING_CAPACITY);
        assert_eq!(lines.first().unwrap(), "line 10");
        assert_eq!(lines.last().unwrap(), "line 59");
    }

    #[test]
    fn file_rotates_at_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        let sink = LogSink::with_file(path.clone(), 64).unwrap();
        for i in 0..20 {
            sink.append(&format!("a fairly long log line number {i}"));
        }
        let rotated = dir.path().join("svc.log.1");
        assert!(rotated.exists(), "expected a rotated generation");
        assert!(path.exists(), "expected the live log to be recreated");
        assert!(std::fs::metadata(&path).unwrap().len() <= 128);
    }
}
