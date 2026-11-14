//! Shared vocabulary of the Isonomic Capsule engine. No process, socket or
//! database code lives here: only the types every other crate agrees on.

pub mod clock;
pub mod config;
pub mod error;
pub mod hardware;
pub mod paths;
pub mod state;

pub use clock::{Clock, ManualClock, SystemClock};
pub use config::EngineConfig;
pub use error::CoreError;
pub use hardware::{FixedHardware, Hardware, HardwareSource, Profile, SystemHardware};
pub use paths::CapsulePaths;
pub use state::{ServiceState, Tier};
