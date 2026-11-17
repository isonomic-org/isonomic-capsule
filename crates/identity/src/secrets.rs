// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where the recovery seed waits for the recovery kit: a small secret,
//! labelled for its Capsule, that never leaves the key store except
//! to be shown to the person once the kit exists.

use std::collections::HashMap;
use std::sync::Mutex;

/// A place that keeps small secrets by name.
pub trait Secrets: Send + Sync {
    /// The secret named `name`, if there is one.
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, String>;
    /// Keep `value` under `name`, replacing nothing: a second put is refused.
    fn put(&self, name: &str, value: &[u8]) -> Result<(), String>;
}

/// Secrets in process memory, for tests.
#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, Vec<u8>>>);

impl Secrets for MemorySecrets {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(name)
            .cloned())
    }

    fn put(&self, name: &str, value: &[u8]) -> Result<(), String> {
        let mut all = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if all.contains_key(name) {
            return Err(format!("{name} is already kept"));
        }
        all.insert(name.to_string(), value.to_vec());
        Ok(())
    }
}

/// The login keychain, under the identity library's own service and name
/// prefix, so it sits beside the keys `id_keys::MacKeyStore` keeps there.
#[cfg(target_os = "macos")]
pub struct MacSecrets;

#[cfg(target_os = "macos")]
const SERVICE: &str = "isonomic-id";

#[cfg(target_os = "macos")]
fn account(name: &str) -> String {
    format!("isonomic-id:{name}")
}

#[cfg(target_os = "macos")]
impl Secrets for MacSecrets {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        use security_framework::passwords::get_generic_password;
        // errSecItemNotFound: nothing kept under that name.
        const NOT_FOUND: i32 = -25300;
        match get_generic_password(SERVICE, &account(name)) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn put(&self, name: &str, value: &[u8]) -> Result<(), String> {
        if self.get(name)?.is_some() {
            return Err(format!("{name} is already kept"));
        }
        security_framework::passwords::set_generic_password(SERVICE, &account(name), value)
            .map_err(|e| e.to_string())
    }
}
