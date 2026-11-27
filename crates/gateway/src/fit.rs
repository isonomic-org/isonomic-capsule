// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whether a model suits this machine, and why, for the model setup's
//! Recommended and Not recommended labels (`models.catalogue`).
//!
//! Four rules, from the machine alone, never from what is free right now:
//! - below [`crate::MEMORY_FLOOR`] no local model is recommended;
//! - a model over the profile's ceiling (`Profile::max_model_bytes`) is not;
//! - a model whose manifest entry asks for more memory (`min_ram_gb`) than
//!   the machine has is not; without that field, one needing more than
//!   [`SHARE`] of the machine's memory while it runs is not;
//! - on a slow machine (one whose models run on the processor alone) a
//!   chat model bigger than [`crate::SLOW_CHAT_MODEL`] is not: it fits,
//!   but answers too slowly to talk to.

use capsule_core::Profile;

const GIB: u64 = 1 << 30;

/// The share of the machine's memory a model may take while it runs and
/// still leave the person's other apps room: three quarters.
pub const SHARE: (u64, u64) = (3, 4);

/// What the rules read about the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineFit {
    pub total_memory_bytes: u64,
    /// The profile's model-size ceiling; `None` is no ceiling.
    pub ceiling_bytes: Option<u64>,
    /// Models run on the processor alone: answers come slowly.
    pub slow: bool,
    /// The largest chat model a slow machine still talks to quickly enough.
    pub slow_limit_bytes: u64,
}

impl MachineFit {
    pub fn new(machine: &crate::Machine, slow: bool, slow_limit_bytes: u64) -> Self {
        let profile = Profile::parse(&machine.profile)
            .unwrap_or_else(|| Profile::from_total_memory(machine.total_memory_bytes));
        Self {
            total_memory_bytes: machine.total_memory_bytes,
            ceiling_bytes: profile.max_model_bytes(),
            slow,
            slow_limit_bytes,
        }
    }
}

/// What a model takes while it runs: its file, the working memory for a
/// conversation (about a fifth more), and the runtime itself.
pub fn running_bytes(size_bytes: u64) -> u64 {
    size_bytes + size_bytes / 5 + GIB / 2
}

/// Whole gigabytes, rounded to the nearest, as a person reads a machine's
/// memory ("16 GB").
fn gb(bytes: u64) -> u64 {
    ((bytes + GIB / 2) / GIB).max(1)
}

/// Whole gigabytes, rounded up, for what a model needs.
fn gb_up(bytes: u64) -> u64 {
    bytes.div_ceil(GIB).max(1)
}

/// Whether a model of `size_bytes` suits the machine, and the sentence or
/// two that says why. `chat` is false for an embedding model; `min_ram_gb`
/// is the manifest's least machine memory for it, where it gives one.
pub fn fit(
    machine: &MachineFit,
    size_bytes: u64,
    chat: bool,
    min_ram_gb: Option<u32>,
) -> (bool, String) {
    let total = machine.total_memory_bytes;
    let has = gb(total);
    let need = running_bytes(size_bytes);
    if total < crate::MEMORY_FLOOR {
        let floor = gb(crate::MEMORY_FLOOR);
        return (
            false,
            format!(
                "This Mac has {has} GB of memory. Local models need a Mac with at least \
                 {floor} GB, so they would slow everything else down."
            ),
        );
    }
    if let Some(min) = min_ram_gb.map(u64::from).filter(|min| has < *min) {
        return (
            false,
            format!(
                "Needs a Mac with at least {min} GB of memory to run well. This Mac has \
                 {has} GB, so other apps would slow down."
            ),
        );
    }
    let over_ceiling = machine.ceiling_bytes.is_some_and(|c| size_bytes > c);
    let over_share = min_ram_gb.is_none() && need * SHARE.1 > total * SHARE.0;
    if over_ceiling || over_share {
        return (
            false,
            format!(
                "Needs about {} GB of memory while it runs. This Mac has {has} GB, so other \
                 apps would slow down.",
                gb_up(need)
            ),
        );
    }
    if !chat {
        return (
            true,
            format!(
                "A small helper model for searching your memory. It runs easily on this \
                 Mac's {has} GB."
            ),
        );
    }
    if machine.slow && size_bytes > machine.slow_limit_bytes {
        return (
            false,
            format!(
                "Fits in this Mac's {has} GB, but this Mac's processor would write its \
                 answers slowly. A smaller model will feel much quicker."
            ),
        );
    }
    // Small: it leaves most of the memory free.
    if need * 4 <= total {
        return (
            true,
            format!("Small enough to answer quickly on this Mac's {has} GB."),
        );
    }
    (
        true,
        format!(
            "Needs about {} GB of memory while it runs, which this Mac's {has} GB holds with \
             room for your other apps.",
            gb_up(need)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(total_gib: u64, slow: bool) -> MachineFit {
        let total = total_gib * GIB;
        MachineFit {
            total_memory_bytes: total,
            ceiling_bytes: Profile::from_total_memory(total).max_model_bytes(),
            slow,
            slow_limit_bytes: 2 * GIB,
        }
    }

    #[test]
    fn below_the_floor_nothing_is_recommended() {
        let (fits, why) = fit(&machine(8, false), GIB / 2, true, None);
        assert!(!fits);
        assert!(why.contains("This Mac has 8 GB"), "{why}");
        assert!(why.contains("at least 16 GB"), "{why}");
        assert!(!fit(&machine(8, false), GIB / 4, false, None).0);
    }

    #[test]
    fn a_small_model_is_quick_on_sixteen_gigabytes() {
        let (fits, why) = fit(&machine(16, false), 2 * GIB, true, None);
        assert!(fits);
        assert_eq!(why, "Small enough to answer quickly on this Mac's 16 GB.");
    }

    #[test]
    fn a_middle_model_fits_with_room_left() {
        let (fits, why) = fit(&machine(16, false), 5 * GIB + GIB / 2, true, None);
        assert!(fits);
        assert!(why.starts_with("Needs about 8 GB"), "{why}");
        assert!(why.contains("16 GB"), "{why}");
    }

    #[test]
    fn a_model_over_the_profiles_ceiling_is_not_recommended() {
        let (fits, why) = fit(&machine(16, false), 20 * GIB, true, None);
        assert!(!fits);
        assert_eq!(
            why,
            "Needs about 25 GB of memory while it runs. This Mac has 16 GB, so other apps \
             would slow down."
        );
        // The same model on a big machine, with no ceiling, fits.
        assert!(fit(&machine(64, false), 20 * GIB, true, None).0);
        // Without a ceiling, the share still holds.
        assert!(!fit(&machine(32, false), 30 * GIB, true, None).0);
    }

    #[test]
    fn a_slow_machine_recommends_only_the_small_chat_models() {
        let slow = machine(16, true);
        let (fits, why) = fit(&slow, 5 * GIB, true, None);
        assert!(!fits);
        assert!(why.contains("processor"), "{why}");
        assert!(fit(&slow, 2 * GIB, true, None).0);
        // Embedding models are small helpers either way.
        assert!(fit(&slow, GIB / 2, false, None).0);
    }

    #[test]
    fn the_manifests_least_memory_decides_where_it_gives_one() {
        let (fits, why) = fit(&machine(16, false), 5 * GIB, true, Some(32));
        assert!(!fits);
        assert_eq!(
            why,
            "Needs a Mac with at least 32 GB of memory to run well. This Mac has 16 GB, so \
             other apps would slow down."
        );
        assert!(fit(&machine(32, false), 5 * GIB, true, Some(32)).0);
        // A big model the manifest allows on 32 GB is not held to the share.
        assert!(fit(&machine(32, false), 22 * GIB, true, Some(32)).0);
        // The floor and the slow rule still hold.
        assert!(!fit(&machine(8, false), GIB, true, Some(8)).0);
        assert!(!fit(&machine(16, true), 5 * GIB, true, Some(16)).0);
    }

    #[test]
    fn the_profile_comes_from_the_machine_when_its_name_is_unknown() {
        let m = crate::Machine {
            profile: "nonsense".into(),
            total_memory_bytes: 16 * GIB,
        };
        let f = MachineFit::new(&m, false, 0);
        assert_eq!(f.ceiling_bytes, Some(10 * GIB));
    }
}
