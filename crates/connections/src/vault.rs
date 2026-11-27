// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a connection's secrets are kept: a pasted token, or a sign-in's
//! tokens. On a Mac that is the login keychain; nothing secret is ever
//! written to the connections file.
//!
//! Unlike the ID's recovery seed, these change: a sign-in's tokens are
//! refreshed, and a removed connection's are deleted.

use std::collections::HashMap;
use std::sync::Mutex;

/// A small secret store, by name.
pub trait Vault: Send + Sync {
    /// The secret kept under `name`, if any.
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, String>;
    /// Keep `value` under `name`, replacing what was there.
    fn set(&self, name: &str, value: &[u8]) -> Result<(), String>;
    /// Forget what is kept under `name`; nothing kept is fine.
    fn delete(&self, name: &str) -> Result<(), String>;
}

/// Secrets in memory, for tests and for a computer with no key store.
#[derive(Default)]
pub struct MemoryVault(Mutex<HashMap<String, Vec<u8>>>);

impl Vault for MemoryVault {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }

    fn set(&self, name: &str, value: &[u8]) -> Result<(), String> {
        self.0.lock().unwrap().insert(name.into(), value.to_vec());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

/// The Mac login keychain, under the service `com.isonomic.connections`.
#[derive(Default)]
pub struct MacVault;

#[cfg(target_os = "macos")]
const SERVICE: &str = "com.isonomic.connections";

#[cfg(target_os = "macos")]
const NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
impl Vault for MacVault {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        match security_framework::passwords::get_generic_password(SERVICE, name) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, name: &str, value: &[u8]) -> Result<(), String> {
        // Replaces an existing item.
        security_framework::passwords::set_generic_password(SERVICE, name, value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        match security_framework::passwords::delete_generic_password(SERVICE, name) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// The names one connection's secrets are kept under.
pub(crate) fn token_name(id: &str) -> String {
    format!("{id}:token")
}

pub(crate) fn sign_in_name(id: &str) -> String {
    format!("{id}:sign-in")
}
