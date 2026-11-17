// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which key store the engine's ID uses: the computer's, unless a scratch
//! Capsule asks for memory.
//!
//! `ISONOMIC_KEY_STORE=memory` keeps the ID's keys in the identity
//! library's in-memory stores for one run, so a throwaway Capsule (the
//! window's self-check) writes nothing to the person's keychain. It is
//! honoured only when the Capsule's `capsule.json` says `"scratch": true`.
//! On a real Capsule it is refused and logged: an ID made there would leave
//! a key log whose keys are gone at the next start.
//!
//! The engine logs which store it chose, in one line; that line is how the
//! window learns it (no wire field).

use crate::{memory_backend, system_backend, OpenBackend};

/// The variable that asks for a store.
pub const ENV_KEY_STORE: &str = "ISONOMIC_KEY_STORE";
/// What the engine logs when it keeps keys in memory.
pub const MEMORY_NOTE: &str = "keys kept in memory: scratch Capsule";

/// Where the keys go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    System,
    Memory,
}

/// The store for `asked` (the variable's value, if set) in a Capsule that
/// is or is not `scratch`, and the line to log, if any.
pub fn choose(asked: Option<&str>, scratch: bool) -> (Store, Option<String>) {
    match asked.map(str::trim) {
        None | Some("") => (Store::System, None),
        Some("memory") if scratch => (Store::Memory, Some(MEMORY_NOTE.to_string())),
        Some("memory") => (
            Store::System,
            Some(format!(
                "{ENV_KEY_STORE}=memory refused: this Capsule is not a scratch Capsule, \
                 so its keys stay in the computer's key store"
            )),
        ),
        Some(other) => (
            Store::System,
            Some(format!(
                "{ENV_KEY_STORE}={other:?} is not a key store (memory); using the computer's"
            )),
        ),
    }
}

/// The backend for a choice.
pub fn backend(store: Store) -> OpenBackend {
    match store {
        Store::System => system_backend(),
        Store::Memory => memory_backend(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_is_honoured_only_on_a_scratch_capsule() {
        assert_eq!(choose(None, true), (Store::System, None));
        assert_eq!(
            choose(Some("memory"), true),
            (Store::Memory, Some(MEMORY_NOTE.into()))
        );
        let (store, note) = choose(Some("memory"), false);
        assert_eq!(store, Store::System);
        assert!(note.unwrap().contains("refused"));
        let (store, note) = choose(Some("disk"), true);
        assert_eq!(store, Store::System);
        assert!(note.unwrap().contains("is not a key store"));
    }
}
