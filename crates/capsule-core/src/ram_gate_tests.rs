use super::*;
use crate::hardware::{FixedHardware, Hardware};
use crate::memory::VmPages;

const GIB: u64 = 1024 * 1024 * 1024;

/// Qwen3.5 2B and 0.8B, as the model library lists them.
const MODEL_2B: u64 = 1_280_835_840;
const MODEL_08B: u64 = 532_517_120;

fn hardware(total: u64, available: u64) -> Arc<dyn HardwareSource> {
    with_pressure(total, available, Pressure::Normal)
}

fn with_pressure(total: u64, available: u64, pressure: Pressure) -> Arc<dyn HardwareSource> {
    Arc::new(FixedHardware(Hardware {
        total_memory_bytes: total,
        available_memory_bytes: available,
        pressure,
    }))
}

#[test]
fn a_model_that_fits_is_admitted() {
    let gate = ProfileRamGate::with_hardware(hardware(16 * GIB, 8 * GIB));
    assert_eq!(gate.profile(), Profile::Standard);
    // 3 GiB model + 1.5 GiB headroom, against 8 GiB available.
    assert!(gate.admit(3 * GIB).is_ok());
}

#[test]
fn a_model_that_does_not_fit_right_now_is_refused_with_the_numbers() {
    // Plenty of total RAM, but almost none to give.
    let gate = ProfileRamGate::with_hardware(hardware(32 * GIB, GIB));
    let denial = gate.admit(3 * GIB).unwrap_err();
    assert!(
        denial.reason.contains("Not enough free memory"),
        "{}",
        denial.reason
    );
    assert_eq!(denial.required_bytes, 3 * GIB + 3 * GIB / 2);
    assert_eq!(denial.available_bytes, GIB);
    assert!(denial.to_string().contains("available"));
}

/// The Intel Mac in ordinary use: about 160 MB free, 5.5 GB inactive. The
/// earlier reading said 1.0 to 1.6 GB available and refused both models.
#[test]
fn the_busy_intel_mac_runs_both_small_models() {
    let pages = VmPages {
        page_size: 4096,
        free: 41_094,
        inactive: 1_403_926,
        speculative: 1_253,
        purgeable: 122_082,
    };
    let available = pages.reclaimable_bytes();
    let gate = ProfileRamGate::with_hardware(hardware(16 * GIB, available));
    assert!(gate.admit(MODEL_2B).is_ok());
    assert!(gate.admit(MODEL_08B).is_ok());
}

/// What the earlier reading reported on that Mac. The new headroom alone
/// would have let the smaller model through; the 2B model still did not fit
/// in 1.07 GB, and the refusal says why in plain words.
#[test]
fn the_earlier_readings_figures_refuse_in_plain_words() {
    let gate = ProfileRamGate::with_hardware(hardware(16 * GIB, 1_068_265_472));
    let denial = gate.admit(MODEL_2B).unwrap_err();
    assert_eq!(denial.required_bytes, MODEL_2B + MODEL_2B / 2);
    assert_eq!(
        denial.reason,
        "Not enough free memory to start this model right now: it needs about 1.9 GB, and 1.1 GB is free. Close some apps or browser tabs, then try again, or choose a smaller model."
    );
    let at_1_6 = ProfileRamGate::with_hardware(hardware(16 * GIB, 1_619_501_056));
    // 0.53 GB + 0.54 GB headroom fits in 1.62 GB.
    assert!(at_1_6.admit(MODEL_08B).is_ok());
}

#[test]
fn critical_pressure_refuses_whatever_the_counts_say() {
    let squeezed = with_pressure(16 * GIB, 6 * GIB, Pressure::Critical);
    let gate = ProfileRamGate::with_hardware(squeezed);
    let reason = gate.admit(MODEL_08B).unwrap_err().reason;
    assert!(reason.contains("short of memory right now"), "{reason}");
    assert!(reason.contains("Close some apps"), "{reason}");
    // A warning alone does not refuse a model that fits.
    let tight = with_pressure(16 * GIB, 6 * GIB, Pressure::Warning);
    assert!(ProfileRamGate::with_hardware(tight).admit(MODEL_2B).is_ok());
}

#[test]
fn headroom_grows_with_the_model_and_has_a_floor() {
    let gate = ProfileRamGate::with_hardware(hardware(16 * GIB, 8 * GIB));
    assert_eq!(gate.headroom_for(MODEL_2B), MODEL_2B / 2);
    assert_eq!(gate.headroom_for(MODEL_08B), MIN_HEADROOM_BYTES);
    assert_eq!(gate.headroom_for(0), MIN_HEADROOM_BYTES);
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
            .contains("too large for this computer (low hardware profile"),
        "{}",
        denial.reason
    );
    // The refusal points somewhere useful rather than just saying no.
    assert!(denial.reason.contains("smaller model"));
}

#[test]
fn the_high_profile_has_no_ceiling_and_defers_entirely_to_free_memory() {
    let gate = ProfileRamGate::with_hardware(hardware(64 * GIB, 60 * GIB));
    assert_eq!(gate.profile(), Profile::High);
    assert!(gate.admit(39 * GIB).is_ok());
    // Still refused when it genuinely will not fit.
    let squeezed = ProfileRamGate::with_hardware(hardware(64 * GIB, 4 * GIB));
    assert!(squeezed.admit(39 * GIB).is_err());
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
    assert!(denial.reason.contains("too large for this computer"));
}

#[test]
fn every_refusal_points_to_a_smaller_model() {
    let ceiling = ProfileRamGate::with_hardware(hardware(8 * GIB, 7 * GIB));
    let busy = ProfileRamGate::with_hardware(hardware(32 * GIB, GIB));
    let pressed =
        ProfileRamGate::with_hardware(with_pressure(32 * GIB, 30 * GIB, Pressure::Critical));
    for denial in [
        ceiling.admit(5 * GIB),
        busy.admit(3 * GIB),
        pressed.admit(GIB),
    ] {
        let reason = denial.unwrap_err().reason;
        assert!(reason.contains("smaller model"), "{reason}");
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
fn a_fixed_headroom_is_configurable_and_actually_applied() {
    let gate = ProfileRamGate::with_hardware(hardware(32 * GIB, 5 * GIB)).with_headroom(GIB);
    assert_eq!(gate.headroom_for(4 * GIB), GIB);
    // 4 GiB + 1 GiB fits in 5 GiB; with half the model on top it would not.
    assert!(gate.admit(4 * GIB).is_ok());
    let proportional = ProfileRamGate::with_hardware(hardware(32 * GIB, 5 * GIB));
    assert!(proportional.admit(4 * GIB).is_err());
}

#[test]
fn the_real_probe_produces_a_usable_gate() {
    // Not asserting a tier: this runs on whatever CI has. Only that a real
    // probe yields something coherent, which is what `detect()` promises.
    let gate = ProfileRamGate::detect();
    assert_eq!(gate.headroom_for(0), MIN_HEADROOM_BYTES);
    let _ = gate.admit(0);
}
