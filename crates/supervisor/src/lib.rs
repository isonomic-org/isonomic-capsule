//! Isonomic Capsule service supervisor.
//!
//! Spec: architecture section 4.2. Carried over from the earlier build with
//! its behaviour and tests, plus three additions: every child's environment
//! is built from an allow-list, a service can listen on a Unix socket, and
//! PostgreSQL gets first-time setup.

pub mod endpoint;
pub mod env;
pub mod health;
pub mod logsink;
mod monitor;
pub mod postgres;
pub mod probe;
pub mod service;
mod spawn;
pub mod stack;

pub use capsule_core::{ServiceState, Tier};
pub use endpoint::Endpoint;
pub use env::ChildEnv;
pub use logsink::LogSink;
pub use probe::Readiness;
pub use service::{ConfigFile, ServiceConfig, ServiceError, ServiceSupervisor, StopSignal};
pub use stack::{Stack, StackService};

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
    fn breaker_trips_after_five() {
        assert!(!should_trip_breaker(5));
        assert!(should_trip_breaker(6));
    }
}
