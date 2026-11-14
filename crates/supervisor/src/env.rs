//! The environment handed to a supervised child, built from an allow-list.
//!
//! The engine clears its own environment at spawn and sets only what is
//! declared here. Nothing from the user's shell reaches a service: no
//! database client variables, no loader paths, no locale surprises.

use std::collections::BTreeMap;

/// Search path for children. Bundled binaries are launched by absolute path;
/// this covers the system tools a service may call.
pub const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChildEnv {
    vars: BTreeMap<String, String>,
}

impl ChildEnv {
    /// No variables at all.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The baseline every service gets: a fixed search path, the C locale and
    /// UTC, so behaviour does not vary with the user's settings.
    pub fn minimal() -> Self {
        Self::empty()
            .with("PATH", SYSTEM_PATH)
            .with("LANG", "C")
            .with("LC_ALL", "C")
            .with("TZ", "UTC")
    }

    /// Declare one variable.
    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// Every declared variable, in name order.
    pub fn vars(&self) -> impl Iterator<Item = (&String, &String)> {
        self.vars.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_is_a_fixed_small_set() {
        let env = ChildEnv::minimal();
        let names: Vec<&str> = env.vars().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["LANG", "LC_ALL", "PATH", "TZ"]);
        assert_eq!(ChildEnv::minimal().get("PATH"), Some(SYSTEM_PATH));
    }

    #[test]
    fn with_adds_and_replaces() {
        let env = ChildEnv::empty()
            .with("A", "1")
            .with("A", "2")
            .with("B", "3");
        assert_eq!(env.get("A"), Some("2"));
        assert_eq!(env.vars().count(), 2);
        assert_eq!(ChildEnv::empty().vars().count(), 0);
    }
}
