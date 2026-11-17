// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The RAM gate (architecture spec, section 4.3), carried over from the
//! earlier build and changed, with the orchestrator's go-ahead, after a 16 GB
//! Intel Mac in ordinary use was refused models it could run.
//!
//! Before a heavy service or a model worker starts, check it against the
//! profile and against the memory the system can give it now, plus working
//! headroom. If it does not fit, refuse with the numbers and plain words,
//! rather than push the machine into swap.
//!
//! Three reasons to refuse, each catching a different failure:
//!
//! 1. **The profile ceiling.** An 8 GB machine should not attempt a 14B model
//!    even on a freshly rebooted machine with everything free. This is a
//!    property of the hardware, not of the moment.
//! 2. **The system under pressure.** When the system itself reports critical
//!    memory pressure, nothing more is loaded, whatever the counts say.
//! 3. **Memory right now.** Even a model within the ceiling must fit in what
//!    the system can give a new process (on macOS, free plus inactive pages;
//!    see `memory`), with headroom on top.
//!
//! Headroom grows with the model: half its size, and never under 512 MiB.
//! The KV cache and the runtime scale with the model; a fixed 2 GiB was more
//! than the small models themselves and refused them on busy machines.
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
use crate::memory::Pressure;

/// The least headroom on top of a model, whatever its size.
pub const MIN_HEADROOM_BYTES: u64 = 512 * 1024 * 1024;

/// Kept for callers of the earlier fixed headroom: the least headroom now.
pub const DEFAULT_HEADROOM_BYTES: u64 = MIN_HEADROOM_BYTES;

/// Why the gate refused to admit a load. Surfaced verbatim, so a refusal
/// always has a clear reason (refuse rather than thrash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamDenial {
    /// Model size plus headroom, in bytes.
    pub required_bytes: u64,
    /// What the system could give a new process, in bytes.
    pub available_bytes: u64,
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

pub trait RamGate: Send + Sync {
    fn admit(&self, model_bytes: u64) -> Result<(), RamDenial>;
}

/// Headroom on top of a model: fixed, or grown with the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Headroom {
    Proportional,
    Fixed(u64),
}

pub struct ProfileRamGate {
    profile: Profile,
    hardware: Arc<dyn HardwareSource>,
    headroom: Headroom,
}

/// Bytes as a person reads them: "1.3 GB".
fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

impl ProfileRamGate {
    pub fn detect() -> Self {
        Self::with_hardware(Arc::new(SystemHardware))
    }

    pub fn with_hardware(hardware: Arc<dyn HardwareSource>) -> Self {
        let profile = Profile::detect(hardware.as_ref());
        Self::new(profile, hardware)
    }

    pub fn new(profile: Profile, hardware: Arc<dyn HardwareSource>) -> Self {
        Self {
            profile,
            hardware,
            headroom: Headroom::Proportional,
        }
    }

    /// A fixed working headroom instead of one that grows with the model.
    /// Mainly a test seam.
    pub fn with_headroom(mut self, headroom_bytes: u64) -> Self {
        self.headroom = Headroom::Fixed(headroom_bytes);
        self
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The headroom on top of a model of `model_bytes`.
    pub fn headroom_for(&self, model_bytes: u64) -> u64 {
        match self.headroom {
            Headroom::Fixed(bytes) => bytes,
            Headroom::Proportional => (model_bytes / 2).max(MIN_HEADROOM_BYTES),
        }
    }
}

impl RamGate for ProfileRamGate {
    fn admit(&self, model_bytes: u64) -> Result<(), RamDenial> {
        let hardware = self.hardware.probe();
        let available_bytes = hardware.available_memory_bytes;
        let required_bytes = model_bytes.saturating_add(self.headroom_for(model_bytes));
        let denial = |reason: String| RamDenial {
            required_bytes,
            available_bytes,
            reason,
        };

        // 1. Profile ceiling: a property of the machine, not of the moment.
        if let Some(ceiling) = self.profile.max_model_bytes() {
            if model_bytes > ceiling {
                return Err(denial(format!(
                    "This model ({}) is too large for this computer ({} hardware profile, up to {}). Choose a smaller model.",
                    gb(model_bytes),
                    self.profile,
                    gb(ceiling)
                )));
            }
        }

        // 2. The system's own word that memory is short.
        if hardware.pressure == Pressure::Critical {
            return Err(denial(
                "Your computer is short of memory right now. Close some apps or browser tabs, then try again, or choose a smaller model."
                    .into(),
            ));
        }

        // 3. What the system can give a new process now.
        if required_bytes > available_bytes {
            return Err(denial(format!(
                "Not enough free memory to start this model right now: it needs about {}, and {} is free. Close some apps or browser tabs, then try again, or choose a smaller model.",
                gb(required_bytes),
                gb(available_bytes)
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "ram_gate_tests.rs"]
mod tests;
