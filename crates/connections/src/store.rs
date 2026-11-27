// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The connections file: `connections.json` in the engine's own folder.
//! Written whole to a temporary file and renamed into place, so a crash
//! leaves the old list or the new one, never half of one.

use crate::record::Record;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Default, Serialize, Deserialize)]
struct File {
    version: u32,
    connections: Vec<Record>,
}

pub(crate) struct Store {
    path: PathBuf,
}

impl Store {
    pub(crate) fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("connections.json"),
        }
    }

    pub(crate) fn load(&self) -> Result<Vec<Record>, String> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<File>(&bytes)
                .map(|f| f.connections)
                .map_err(|e| format!("the connections file could not be read: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("the connections file could not be read: {e}")),
        }
    }

    pub(crate) fn save(&self, records: &[Record]) -> Result<(), String> {
        let file = File {
            version: 1,
            connections: records.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
}
