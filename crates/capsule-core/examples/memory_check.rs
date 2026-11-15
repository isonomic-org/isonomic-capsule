//! What the RAM gate sees on this machine now, and what it would answer for
//! the two small chat models. Not part of the test suite.
//!
//!     cargo run -q -p capsule-core --example memory_check

use capsule_core::{HardwareSource, ProfileRamGate, RamGate, SystemHardware};

fn main() {
    let hardware = SystemHardware.probe();
    let gb = |b: u64| b as f64 / 1e9;
    println!("total memory: {:.1} GB", gb(hardware.total_memory_bytes));
    println!(
        "available now: {:.2} GB",
        gb(hardware.available_memory_bytes)
    );
    println!("pressure: {:?}", hardware.pressure);
    let gate = ProfileRamGate::detect();
    println!("profile: {}", gate.profile());
    for (name, bytes) in [
        ("Qwen3.5 2B", 1_280_835_840u64),
        ("Qwen3.5 0.8B", 532_517_120),
    ] {
        match gate.admit(bytes) {
            Ok(()) => println!(
                "{name}: admitted (needs {:.2} GB with headroom)",
                gb(bytes + gate.headroom_for(bytes))
            ),
            Err(denial) => println!("{name}: refused: {}", denial.reason),
        }
    }
}
