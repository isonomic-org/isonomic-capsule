//! The RAM gate (architecture spec, section 4.3), carried over from the
//! earlier build.
//!
//! Before a heavy service or a model worker starts, check it against the
//! profile and against free physical memory plus working headroom. If it does
//! not fit, refuse with the numbers rather than push the machine into swap.
//!
//! Two independent reasons to refuse, and they catch different failures:
//!
//! 1. **The profile ceiling.** An 8 GB machine should not attempt a 14B model
//!    even on a freshly rebooted machine with everything free. This is a
//!    property of the hardware, not of the moment.
//! 2. **Free memory right now.** Even a model within the ceiling must still fit
//!    alongside whatever else is running, with working headroom on top.
//!
//! The refusal has exactly the fields of the SDK's `memory` error
//! (`required_bytes`, `available_bytes`, `reason`), so the broker passes it on
//! unchanged. The inference workspace declares the same seam and leaves the
//! hardware-aware gate to the engine: this is that gate, not a second copy.
//!
//! Memory readings come through an injectable source, so these tests do not
//! depend on whatever the CI runner happens to have free.

use std::fmt;
use std::sync::Arc;

use crate::hardware::{HardwareSource, Profile, SystemHardware};

/// Working headroom reserved on top of the model's on-disk size: KV cache,
/// the runtime's own allocations, and enough slack that the OS is not pushed
/// into swap the moment inference starts.
pub const DEFAULT_HEADROOM_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Why the gate refused to admit a load. Surfaced verbatim, so a refusal
/// always has a clear reason (refuse rather than thrash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamDenial {
    /// Model size plus headroom, in bytes.
    pub required_bytes: u64,
    /// Available physical memory at decision time, in bytes.
    pub available_bytes: u64,
    /// Human-readable reason for the refusal.
    pub reason: String,
}

impl fmt::Display for RamDenial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (required {} bytes incl. headroom, available {} bytes)",
            self.reason, self.required_bytes, self.available_bytes
        )
    }
}

/// The seam consulted before starting a heavy service or a model worker.
pub trait RamGate: Send + Sync {
    /// May something of this size (a model's on-disk size, or a service's
    /// declared footprint) be loaded right now?
    fn admit(&self, model_bytes: u64) -> Result<(), RamDenial>;
}

/// The profile-aware budgeter.
pub struct ProfileRamGate {
    profile: Profile,
    hardware: Arc<dyn HardwareSource>,
    headroom_bytes: u64,
}

impl ProfileRamGate {
    /// Probe the machine, pick a profile, and gate against it.
    pub fn detect() -> Self {
        Self::with_hardware(Arc::new(SystemHardware))
    }

    /// Detect the profile from an injected hardware source.
    pub fn with_hardware(hardware: Arc<dyn HardwareSource>) -> Self {
        let profile = Profile::detect(hardware.as_ref());
        Self::new(profile, hardware)
    }

    /// Gate against an explicit profile — the path a user override takes, where
    /// the chosen profile must win over what the probe would have said.
    pub fn new(profile: Profile, hardware: Arc<dyn HardwareSource>) -> Self {
        Self {
            profile,
            hardware,
            headroom_bytes: DEFAULT_HEADROOM_BYTES,
        }
    }

    /// Override the working headroom. Mainly a test seam; production uses the
    /// default.
    pub fn with_headroom(mut self, headroom_bytes: u64) -> Self {
        self.headroom_bytes = headroom_bytes;
        self
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    pub fn headroom_bytes(&self) -> u64 {
        self.headroom_bytes
    }
}

impl RamGate for ProfileRamGate {
    fn admit(&self, model_bytes: u64) -> Result<(), RamDenial> {
        let available_bytes = self.hardware.probe().available_memory_bytes;
        let required_bytes = model_bytes.saturating_add(self.headroom_bytes);

        // 1. Profile ceiling — a property of the machine, not of the moment.
        if let Some(ceiling) = self.profile.max_model_bytes() {
            if model_bytes > ceiling {
                return Err(RamDenial {
                    required_bytes,
                    available_bytes,
                    reason: format!(
                        "this model is too large for the {} hardware profile: {model_bytes} B exceeds the {ceiling} B ceiling; try a smaller model or the remote provider",
                        self.profile
                    ),
                });
            }
        }

        // 2. Free memory right now.
        if required_bytes > available_bytes {
            return Err(RamDenial {
                required_bytes,
                available_bytes,
                reason: format!(
                    "not enough free RAM to load the model without thrashing: model {model_bytes} B + headroom {} B exceeds available {available_bytes} B; try a smaller model or a remote one",
                    self.headroom_bytes
                ),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{FixedHardware, Hardware};

    const GIB: u64 = 1024 * 1024 * 1024;

    fn hardware(total: u64, available: u64) -> Arc<dyn HardwareSource> {
        Arc::new(FixedHardware(Hardware {
            total_memory_bytes: total,
            available_memory_bytes: available,
        }))
    }

    #[test]
    fn a_model_that_fits_is_admitted() {
        let gate = ProfileRamGate::with_hardware(hardware(16 * GIB, 8 * GIB));
        assert_eq!(gate.profile(), Profile::Standard);
        // 3 GiB model + 2 GiB headroom = 5 GiB, against 8 GiB available.
        assert!(gate.admit(3 * GIB).is_ok());
    }

    #[test]
    fn a_model_that_does_not_fit_right_now_is_refused_with_the_numbers() {
        // Plenty of total RAM, but almost none free.
        let gate = ProfileRamGate::with_hardware(hardware(32 * GIB, GIB));
        let denial = gate.admit(3 * GIB).unwrap_err();
        assert!(denial.reason.contains("not enough free RAM"));
        assert_eq!(denial.required_bytes, 3 * GIB + DEFAULT_HEADROOM_BYTES);
        assert_eq!(denial.available_bytes, GIB);
        assert!(denial.to_string().contains("available"));
    }

    /// The failure the provisional gate could not see: an 8 GB machine that has
    /// just booted has enough *free* memory for a big model, and would have
    /// been allowed to load one it cannot sustain.
    #[test]
    fn a_low_profile_machine_refuses_an_oversized_model_even_when_memory_is_free() {
        let gate = ProfileRamGate::with_hardware(hardware(8 * GIB, 7 * GIB));
        assert_eq!(gate.profile(), Profile::Low);
        let denial = gate.admit(5 * GIB).unwrap_err();
        assert!(
            denial
                .reason
                .contains("too large for the low hardware profile"),
            "{}",
            denial.reason
        );
        // The refusal points somewhere useful rather than just saying no.
        assert!(denial.reason.contains("remote provider"));
    }

    #[test]
    fn the_high_profile_has_no_ceiling_and_defers_entirely_to_free_memory() {
        let gate = ProfileRamGate::with_hardware(hardware(64 * GIB, 60 * GIB));
        assert_eq!(gate.profile(), Profile::High);
        assert!(gate.admit(40 * GIB).is_ok());
        // Still refused when it genuinely will not fit.
        let squeezed = ProfileRamGate::with_hardware(hardware(64 * GIB, 4 * GIB));
        assert!(squeezed.admit(40 * GIB).is_err());
    }

    #[test]
    fn an_explicit_profile_overrides_what_the_probe_would_have_chosen() {
        // A 64 GiB machine forced to the low profile must behave as low.
        let gate = ProfileRamGate::new(Profile::Low, hardware(64 * GIB, 60 * GIB));
        assert_eq!(gate.profile(), Profile::Low);
        assert!(gate.admit(5 * GIB).is_err());
    }

    #[test]
    fn the_ceiling_is_checked_before_free_memory() {
        // Both conditions fail; the profile ceiling is the more useful message
        // because it is stable, whereas free memory changes minute to minute.
        let gate = ProfileRamGate::with_hardware(hardware(8 * GIB, GIB));
        let denial = gate.admit(6 * GIB).unwrap_err();
        assert!(denial
            .reason
            .contains("too large for the low hardware profile"));
    }

    #[test]
    fn both_refusals_point_to_a_smaller_model_or_a_remote_one() {
        let ceiling = ProfileRamGate::with_hardware(hardware(8 * GIB, 7 * GIB));
        let busy = ProfileRamGate::with_hardware(hardware(32 * GIB, GIB));
        for denial in [ceiling.admit(5 * GIB), busy.admit(3 * GIB)] {
            let reason = denial.unwrap_err().reason;
            assert!(reason.contains("smaller model"), "{reason}");
            assert!(reason.contains("remote"), "{reason}");
        }
    }

    #[test]
    fn an_absurd_model_size_does_not_overflow_the_headroom_addition() {
        let gate = ProfileRamGate::new(Profile::High, hardware(64 * GIB, 60 * GIB));
        let denial = gate.admit(u64::MAX).unwrap_err();
        assert_eq!(denial.required_bytes, u64::MAX);
        assert!(denial.required_bytes > denial.available_bytes);
    }

    #[test]
    fn headroom_is_configurable_and_actually_applied() {
        let gate = ProfileRamGate::with_hardware(hardware(32 * GIB, 5 * GIB)).with_headroom(GIB);
        assert_eq!(gate.headroom_bytes(), GIB);
        // 4 GiB + 1 GiB headroom fits in 5 GiB; with the 2 GiB default it would not.
        assert!(gate.admit(4 * GIB).is_ok());
        let default = ProfileRamGate::with_hardware(hardware(32 * GIB, 5 * GIB));
        assert!(default.admit(4 * GIB).is_err());
    }

    #[test]
    fn the_real_probe_produces_a_usable_gate() {
        // Not asserting a tier: this runs on whatever CI has. Only that a real
        // probe yields something coherent, which is what `detect()` promises.
        let gate = ProfileRamGate::detect();
        assert_eq!(gate.headroom_bytes(), DEFAULT_HEADROOM_BYTES);
        // A zero-byte model is refused only if the machine has under 2 GiB free.
        let _ = gate.admit(0);
    }
}
