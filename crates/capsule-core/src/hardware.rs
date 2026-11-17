// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Hardware profiles (architecture spec, section 4.3).
//!
//! One profile, chosen from physical memory, is the single source of every
//! tunable: database buffers, cache memory, model idle unload, the cap on
//! loaded apps, and the model-size ceiling the RAM gate enforces.
//!
//! The profile is data, not behaviour: it hands values to whoever renders a
//! service's configuration. Nothing here spawns or decides anything.
//! The machine is read through `HardwareSource`, so tests inject their own.

use std::fmt;

use crate::memory::Pressure;
use std::time::Duration;

const GIB: u64 = 1024 * 1024 * 1024;

/// What the probe actually measured.
///
/// RAM only, deliberately. Every value in the profile table is driven by
/// memory; cores, GPU, VRAM and disk drive none of them today, and probing
/// them per OS would add API surface nothing uses yet. They are recorded as a
/// gap in the area's status file rather than guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hardware {
    pub total_memory_bytes: u64,
    /// What a new process can have now without paging anything out.
    pub available_memory_bytes: u64,
    /// The system's own memory pressure, where it reports one.
    pub pressure: Pressure,
}

/// Where hardware facts come from. Injectable so profile selection and the RAM
/// gate are testable without depending on whatever the CI runner happens to
/// have free.
pub trait HardwareSource: Send + Sync {
    fn probe(&self) -> Hardware;
}

/// The real probe.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemHardware;

impl HardwareSource for SystemHardware {
    fn probe(&self) -> Hardware {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let total_memory_bytes = sys.total_memory();
        // On macOS the kernel's own page counts: the library's figure there
        // leaves out the inactive pages the system hands out on demand.
        #[cfg(target_os = "macos")]
        if let Some((pages, pressure)) = crate::memory::read_macos() {
            return Hardware {
                total_memory_bytes,
                available_memory_bytes: pages.reclaimable_bytes().min(total_memory_bytes),
                pressure,
            };
        }
        Hardware {
            total_memory_bytes,
            available_memory_bytes: sys.available_memory(),
            pressure: Pressure::Unknown,
        }
    }
}

/// A fixed reading, for tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedHardware(pub Hardware);

impl FixedHardware {
    /// Both totals equal — the common case when only the tier matters.
    pub fn with_total(total_memory_bytes: u64) -> Self {
        Self(Hardware {
            total_memory_bytes,
            available_memory_bytes: total_memory_bytes,
            pressure: Pressure::Normal,
        })
    }
}

impl HardwareSource for FixedHardware {
    fn probe(&self) -> Hardware {
        self.0
    }
}

/// The three profiles from the spec table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Profile {
    /// 8 GB class.
    Low,
    /// 16 GB class.
    Standard,
    /// 32 GB and above.
    High,
}

impl Profile {
    /// Machines report slightly less than their nominal size, and RAM is sold in
    /// round numbers, so the boundaries sit in the gaps rather than on the
    /// nominal values: under 12 GiB is the 8 GB class, under 24 GiB the 16 GB
    /// class, the rest is 32 GB+.
    pub fn from_total_memory(total_memory_bytes: u64) -> Profile {
        if total_memory_bytes < 12 * GIB {
            Profile::Low
        } else if total_memory_bytes < 24 * GIB {
            Profile::Standard
        } else {
            Profile::High
        }
    }

    /// Probe the machine and pick a profile.
    pub fn detect(source: &dyn HardwareSource) -> Profile {
        Profile::from_total_memory(source.probe().total_memory_bytes)
    }

    /// Postgres `shared_buffers`, rendered into `postgresql.conf`.
    pub fn shared_buffers(&self) -> &'static str {
        match self {
            Profile::Low => "128MB",
            Profile::Standard => "512MB",
            Profile::High => "1GB",
        }
    }

    /// Cache `maxmemory`. Paired with an LRU policy on the low profile, where
    /// eviction is expected rather than exceptional.
    pub fn cache_maxmemory(&self) -> &'static str {
        match self {
            Profile::Low => "256mb",
            Profile::Standard => "1gb",
            Profile::High => "2gb",
        }
    }

    /// How long a local model may sit idle before it is unloaded. `None` on the
    /// high profile: never unload.
    pub fn model_idle_unload(&self) -> Option<Duration> {
        match self {
            Profile::Low => Some(Duration::from_secs(5 * 60)),
            Profile::Standard => Some(Duration::from_secs(15 * 60)),
            Profile::High => None,
        }
    }

    /// How many apps may run at once. `None` is unlimited.
    pub fn max_concurrent_apps(&self) -> Option<usize> {
        match self {
            Profile::Low => Some(2),
            Profile::Standard => Some(4),
            Profile::High => None,
        }
    }

    /// The largest model this profile will admit, from the table's "default local
    /// model" row: Q4 3B–8B (≤4 GB) on low, Q4/Q5 8B–14B on standard, user
    /// choice on high. This is a *ceiling*, not a recommendation — the RAM gate
    /// also has to find the memory free at the time.
    pub fn max_model_bytes(&self) -> Option<u64> {
        match self {
            Profile::Low => Some(4 * GIB),
            Profile::Standard => Some(10 * GIB),
            Profile::High => None,
        }
    }

    /// Stable string form for the service registry and for a user override.
    pub fn as_str(&self) -> &'static str {
        match self {
            Profile::Low => "low",
            Profile::Standard => "standard",
            Profile::High => "high",
        }
    }

    /// Inverse of `as_str`. Unknown values are `None` rather than a silent
    /// default: a corrupted override should fall back to a fresh probe, not to
    /// a guess that might over-commit the machine.
    pub fn parse(value: &str) -> Option<Profile> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Profile::Low),
            "standard" => Some(Profile::Standard),
            "high" => Some(Profile::High),
            _ => None,
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nominal_machine_sizes_land_in_the_intended_tier() {
        for (gib, expected) in [
            (8, Profile::Low),
            (16, Profile::Standard),
            (32, Profile::High),
            (64, Profile::High),
        ] {
            assert_eq!(
                Profile::from_total_memory(gib * GIB),
                expected,
                "{gib} GiB machine"
            );
        }
    }

    #[test]
    fn a_machine_reporting_slightly_under_its_nominal_size_still_lands_right() {
        // Real machines report a little less than nominal (firmware reserves).
        let just_under_8 = 8 * GIB - 400 * 1024 * 1024;
        assert_eq!(Profile::from_total_memory(just_under_8), Profile::Low);
        let just_under_16 = 16 * GIB - 400 * 1024 * 1024;
        assert_eq!(Profile::from_total_memory(just_under_16), Profile::Standard);
        let just_under_32 = 32 * GIB - 400 * 1024 * 1024;
        assert_eq!(Profile::from_total_memory(just_under_32), Profile::High);
    }

    #[test]
    fn boundaries_are_where_the_comment_says_they_are() {
        assert_eq!(Profile::from_total_memory(12 * GIB - 1), Profile::Low);
        assert_eq!(Profile::from_total_memory(12 * GIB), Profile::Standard);
        assert_eq!(Profile::from_total_memory(24 * GIB - 1), Profile::Standard);
        assert_eq!(Profile::from_total_memory(24 * GIB), Profile::High);
    }

    #[test]
    fn a_tiny_or_absent_reading_is_treated_as_the_most_conservative_profile() {
        assert_eq!(Profile::from_total_memory(0), Profile::Low);
    }

    #[test]
    fn detect_reads_through_the_injected_source() {
        let low = FixedHardware::with_total(8 * GIB);
        assert_eq!(Profile::detect(&low), Profile::Low);
        let high = FixedHardware::with_total(64 * GIB);
        assert_eq!(Profile::detect(&high), Profile::High);
    }

    #[test]
    fn every_value_matches_the_spec_table() {
        assert_eq!(Profile::Low.shared_buffers(), "128MB");
        assert_eq!(Profile::Standard.shared_buffers(), "512MB");
        assert_eq!(Profile::High.shared_buffers(), "1GB");

        assert_eq!(Profile::Low.cache_maxmemory(), "256mb");
        assert_eq!(Profile::Standard.cache_maxmemory(), "1gb");
        assert_eq!(Profile::High.cache_maxmemory(), "2gb");

        assert_eq!(
            Profile::Low.model_idle_unload(),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            Profile::Standard.model_idle_unload(),
            Some(Duration::from_secs(900))
        );
        assert_eq!(Profile::High.model_idle_unload(), None);

        assert_eq!(Profile::Low.max_concurrent_apps(), Some(2));
        assert_eq!(Profile::Standard.max_concurrent_apps(), Some(4));
        assert_eq!(Profile::High.max_concurrent_apps(), None);

        assert_eq!(Profile::Low.max_model_bytes(), Some(4 * GIB));
        assert!(Profile::High.max_model_bytes().is_none());
    }

    #[test]
    fn limits_loosen_monotonically_as_the_machine_gets_bigger() {
        // A bigger machine must never be given a tighter budget than a smaller
        // one; this catches a typo'd table entry that individual asserts miss.
        let tiers = [Profile::Low, Profile::Standard, Profile::High];
        for pair in tiers.windows(2) {
            let (smaller, bigger) = (pair[0], pair[1]);
            let apps = |p: Profile| p.max_concurrent_apps().unwrap_or(usize::MAX);
            assert!(apps(bigger) >= apps(smaller), "{bigger} vs {smaller}: apps");
            let model = |p: Profile| p.max_model_bytes().unwrap_or(u64::MAX);
            assert!(
                model(bigger) >= model(smaller),
                "{bigger} vs {smaller}: model"
            );
            let idle = |p: Profile| p.model_idle_unload().unwrap_or(Duration::MAX);
            assert!(idle(bigger) >= idle(smaller), "{bigger} vs {smaller}: idle");
        }
    }

    #[test]
    fn profile_strings_round_trip_and_reject_nonsense() {
        for profile in [Profile::Low, Profile::Standard, Profile::High] {
            assert_eq!(Profile::parse(profile.as_str()), Some(profile));
            assert_eq!(profile.to_string(), profile.as_str());
        }
        assert_eq!(Profile::parse("HIGH"), Some(Profile::High));
        assert_eq!(Profile::parse("  low  "), Some(Profile::Low));
        // A corrupted override must not silently become a real profile.
        assert_eq!(Profile::parse("enormous"), None);
        assert_eq!(Profile::parse(""), None);
    }
}
