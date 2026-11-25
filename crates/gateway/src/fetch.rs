// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Fetching a model file so that a person's first download survives what a
//! laptop does to it: being quit, put to sleep, or losing its connection.
//!
//! The file arrives as `<name>.part` beside its final place. A later attempt
//! continues the part with a range request, re-hashing what is already on
//! disk, and starts over only if the server ignores the range. A transfer
//! that stops sending for the stall limit fails and keeps its part. The file
//! is renamed into place only once its SHA-256 matches the manifest, so a
//! half file is never installed: a model counts as installed only under its
//! final name (see `Catalogue::installed`).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use isonomic_model_library::ModelSpec;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// How long a transfer may send nothing before it fails. Long enough for
/// a slow connection, short enough that a dead one does not look alive.
pub const STALL_LIMIT: Duration = Duration::from_secs(60);

/// The wait after which a silent transfer counts as stalled, made afresh
/// for every chunk. Tests end it on demand instead of waiting.
pub type Stall = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The real stall wait: `STALL_LIMIT` of silence.
pub fn stall_limit() -> Stall {
    Arc::new(|| Box::pin(tokio::time::sleep(STALL_LIMIT)))
}

/// Why a fetch ended without the file.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("the download stopped: {0}")]
    Network(#[from] reqwest::Error),
    #[error("the connection stopped sending")]
    Stalled,
    #[error("the downloaded file did not match its checksum, so it was discarded")]
    Checksum,
    #[error("the server sent {received} bytes where {expected} were expected")]
    Size { received: u64, expected: u64 },
    #[error("could not write the model file: {0}")]
    Io(#[from] std::io::Error),
}

/// Bytes on disk and the expected total, as a fetch goes.
pub type Progress<'a> = &'a mut (dyn FnMut(u64, Option<u64>) + Send);

/// The part file for `spec` in `dir`.
pub fn part_path(dir: &Path, spec: &ModelSpec) -> PathBuf {
    dir.join(format!("{}.part", spec.file_name))
}

/// How many bytes an earlier attempt left in the part file.
pub fn part_len(dir: &Path, spec: &ModelSpec) -> u64 {
    std::fs::metadata(part_path(dir, spec)).map_or(0, |m| m.len())
}

/// Fetch `spec` into `dir`, continuing an earlier attempt's part.
pub async fn fetch(
    client: &reqwest::Client,
    spec: &ModelSpec,
    dir: &Path,
    stall: &Stall,
    progress: Progress<'_>,
) -> Result<PathBuf, FetchError> {
    tokio::fs::create_dir_all(dir).await?;
    let dest = dir.join(&spec.file_name);
    let part = part_path(dir, spec);
    let expected = Some(spec.size_bytes).filter(|s| *s > 0);

    let mut hasher = Sha256::new();
    let mut offset = hash_part(&part, &mut hasher).await?;
    if expected.is_some_and(|e| offset > e) {
        // Longer than the model: not a prefix of it.
        offset = restart(&part, &mut hasher).await?;
    }
    if expected == Some(offset) {
        // A whole file from an attempt cut off before its rename.
        return finish(&part, &dest, hasher, offset, spec);
    }

    let mut request = client.get(&spec.url);
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let response = request.send().await?;
    let resumed = offset > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let response = response.error_for_status()?;
    if offset > 0 && !resumed {
        // The server sent the whole file from the start.
        offset = restart(&part, &mut hasher).await?;
    }
    let total = expected.or(response.content_length().map(|l| l + offset));
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part)
        .await?;
    let mut received = offset;
    progress(received, total);
    let mut stream = response.bytes_stream();
    let outcome = loop {
        let next = tokio::select! {
            next = stream.next() => next,
            () = stall() => break Err(FetchError::Stalled),
        };
        match next {
            None => break Ok(()),
            Some(Err(e)) => break Err(e.into()),
            Some(Ok(chunk)) => {
                if let Err(e) = file.write_all(&chunk).await {
                    break Err(e.into());
                }
                hasher.update(&chunk);
                received += chunk.len() as u64;
                progress(received, total);
            }
        }
    };
    // Whatever arrived stays on disk for the next attempt.
    file.flush().await?;
    file.sync_data().await?;
    drop(file);
    outcome?;
    finish(&part, &dest, hasher, received, spec)
}

/// Check the whole part and move it into place, or discard it.
fn finish(
    part: &Path,
    dest: &Path,
    hasher: Sha256,
    received: u64,
    spec: &ModelSpec,
) -> Result<PathBuf, FetchError> {
    let expected = Some(spec.size_bytes).filter(|s| *s > 0);
    // Short: the connection ended early; the part stays for the next try.
    if let Some(expected) = expected.filter(|e| *e != received) {
        return Err(FetchError::Size { received, expected });
    }
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(&spec.sha256_hex) {
        std::fs::remove_file(part)?;
        return Err(FetchError::Checksum);
    }
    std::fs::rename(part, dest)?;
    Ok(dest.to_path_buf())
}

/// Feed the part's bytes to `hasher`; how many there were.
async fn hash_part(part: &Path, hasher: &mut Sha256) -> std::io::Result<u64> {
    let mut file = match tokio::fs::File::open(part).await {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut buf = vec![0u8; 1 << 20];
    let mut len = 0;
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            return Ok(len);
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
}

/// Throw the part away and start the hash again.
async fn restart(part: &Path, hasher: &mut Sha256) -> std::io::Result<u64> {
    match tokio::fs::remove_file(part).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    *hasher = Sha256::new();
    Ok(0)
}
