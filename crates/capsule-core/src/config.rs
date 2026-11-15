//! Engine configuration. Everything comes from explicit inputs: the engine
//! never guesses a path from its working directory.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::error::CoreError;
use crate::hardware::Profile;
use crate::paths::{CapsulePaths, ENV_CAPSULE_DIR};

/// Directory holding the bundled service binaries.
pub const ENV_BUNDLE_DIR: &str = "ISONOMIC_BUNDLE_DIR";
/// Forces a hardware profile (`low`, `standard`, `high`).
pub const ENV_PROFILE: &str = "ISONOMIC_PROFILE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    pub paths: CapsulePaths,
    /// Where bundled binaries live. `None` means "next to the engine binary",
    /// which the packaging crate resolves.
    pub bundle_dir: Option<PathBuf>,
    /// A user override of the hardware profile. An unreadable value is
    /// dropped, which triggers a fresh probe.
    pub profile_override: Option<Profile>,
    /// The override as given, when it could not be read, so the engine can
    /// say why it probed instead.
    pub profile_override_invalid: Option<String>,
}

impl EngineConfig {
    /// Build the configuration from a variable lookup and a home directory.
    /// Pure, so tests pass their own lookup.
    pub fn resolve(
        var: impl Fn(&str) -> Option<OsString>,
        home: Option<PathBuf>,
    ) -> Result<Self, CoreError> {
        let paths = CapsulePaths::resolve(var(ENV_CAPSULE_DIR), home)?;
        let bundle_dir = match var(ENV_BUNDLE_DIR).map(PathBuf::from) {
            Some(dir) if dir.is_absolute() => Some(dir),
            Some(dir) => {
                return Err(CoreError::RelativePath {
                    name: ENV_BUNDLE_DIR,
                    value: dir,
                })
            }
            None => None,
        };
        let raw = var(ENV_PROFILE).map(|v| v.to_string_lossy().into_owned());
        let profile_override = raw.as_deref().and_then(Profile::parse);
        let profile_override_invalid = raw.filter(|_| profile_override.is_none());
        Ok(Self {
            paths,
            bundle_dir,
            profile_override,
            profile_override_invalid,
        })
    }

    /// The configuration for this process.
    pub fn from_environment() -> Result<Self, CoreError> {
        Self::resolve(|name| std::env::var_os(name), home_dir())
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn override_wins_and_bundle_dir_is_optional() {
        let config = EngineConfig::resolve(
            lookup(&[(ENV_CAPSULE_DIR, "/data/capsule")]),
            Some(PathBuf::from("/home/u")),
        )
        .unwrap();
        assert_eq!(config.paths.root(), std::path::Path::new("/data/capsule"));
        assert_eq!(config.bundle_dir, None);
        assert_eq!(config.profile_override, None);
    }

    #[test]
    fn a_relative_bundle_dir_is_rejected() {
        let result = EngineConfig::resolve(
            lookup(&[(ENV_CAPSULE_DIR, "/c"), (ENV_BUNDLE_DIR, "bin")]),
            None,
        );
        assert!(matches!(result, Err(CoreError::RelativePath { .. })));
    }

    #[test]
    fn an_unreadable_profile_override_is_dropped() {
        let good = EngineConfig::resolve(
            lookup(&[(ENV_CAPSULE_DIR, "/c"), (ENV_PROFILE, " low ")]),
            None,
        );
        let good = good.unwrap();
        assert_eq!(good.profile_override, Some(Profile::Low));
        assert_eq!(good.profile_override_invalid, None);
        let bad = EngineConfig::resolve(
            lookup(&[(ENV_CAPSULE_DIR, "/c"), (ENV_PROFILE, "huge")]),
            None,
        );
        let bad = bad.unwrap();
        assert_eq!(bad.profile_override, None);
        assert_eq!(bad.profile_override_invalid.as_deref(), Some("huge"));
    }
}
