// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Errors shared across the engine's crates.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("cannot locate the Capsule directory: no home directory and {0} is not set")]
    NoHome(&'static str),
    #[error("{name} must be an absolute path, got {value:?}")]
    RelativePath { name: &'static str, value: PathBuf },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
