//! Isonomic Capsule service supervisor.
//!
//! Spec: architecture section 4.2. Carried over from the earlier build with
//! its behaviour and tests, plus three additions: every child's environment
//! is built from an allow-list, a service can listen on a Unix socket, and
//! PostgreSQL gets first-time setup.

pub mod cache;
pub mod endpoint;
pub mod env;
mod guardian;
pub mod health;
pub mod logsink;
mod monitor;
mod orphan;
pub mod postgres;
pub mod probe;
pub mod service;
mod spawn;
pub mod stack;
pub mod vector;

use std::time::Duration;

pub use capsule_core::{ServiceState, Tier};
pub use endpoint::Endpoint;
pub use env::ChildEnv;
pub use logsink::LogSink;
pub use probe::Readiness;
pub use service::{ConfigFile, ServiceConfig, ServiceError, ServiceSupervisor, StopSignal};
pub use stack::{LazyError, Stack, StackService};

/// First wait between readiness probes while a service starts.
pub const FIRST_STARTING_PROBE: Duration = Duration::from_millis(50);

/// Wait before the next readiness probe while a service starts: 50 ms,
/// doubling, up to `max` (the configured probe interval, 5 s by default).
/// A fast service is seen ready within a few tens of milliseconds instead of
/// a full interval later; a slow one is still probed no more often than the
/// ramp allows.
pub fn starting_probe_delay(attempt: u32, max: Duration) -> Duration {
    let factor = 1u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX);
    FIRST_STARTING_PROBE.saturating_mul(factor).min(max)
}

/// Exponential backoff for restarts: 1s, 2s, 4s and so on, capped at 60s.
pub fn restart_backoff_secs(attempt: u32) -> u64 {
    let raw = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    raw.min(60)
}

/// Circuit breaker: more than 5 restarts inside a 10-minute window is Degraded.
pub fn should_trip_breaker(restarts_in_window: u32) -> bool {
    restarts_in_window > 5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_caps_at_60() {
        assert_eq!(restart_backoff_secs(0), 1);
        assert_eq!(restart_backoff_secs(1), 2);
        assert_eq!(restart_backoff_secs(2), 4);
        assert_eq!(restart_backoff_secs(5), 32);
        assert_eq!(restart_backoff_secs(6), 60);
        assert_eq!(restart_backoff_secs(63), 60);
        assert_eq!(restart_backoff_secs(64), 60); // overflow guard
    }

    #[test]
    fn starting_probes_ramp_from_50ms_to_the_interval() {
        let max = Duration::from_secs(5);
        let delays: Vec<u64> = (0..9)
            .map(|attempt| starting_probe_delay(attempt, max).as_millis() as u64)
            .collect();
        assert_eq!(delays, [50, 100, 200, 400, 800, 1600, 3200, 5000, 5000]);
        assert_eq!(starting_probe_delay(u32::MAX, max), max);
        // A shorter configured interval caps the ramp from the start.
        let short = Duration::from_millis(25);
        assert_eq!(starting_probe_delay(0, short), short);
    }

    #[test]
    fn breaker_trips_after_five() {
        assert!(!should_trip_breaker(5));
        assert!(should_trip_breaker(6));
    }
}
