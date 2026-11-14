//! Startup tiers and lifecycle states, shared by the registry, the supervisor
//! and the broker.

/// Startup tier of a managed service (architecture spec, section 4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// In-process parts of the engine: started immediately.
    T0,
    /// Core services: started in parallel right after tier 0.
    T1,
    /// Heavy services: started on first use.
    T2,
    /// App functions and Cells: started once the services they declare are healthy.
    T3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceState {
    Stopped,
    Starting,
    Healthy,
    /// Circuit breaker tripped: more than 5 restarts in 10 minutes.
    Degraded,
}

impl ServiceState {
    /// Stable string form used by the registry and on the wire.
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceState::Stopped => "stopped",
            ServiceState::Starting => "starting",
            ServiceState::Healthy => "healthy",
            ServiceState::Degraded => "degraded",
        }
    }

    /// One light for many services: Degraded if any is, Healthy only if all
    /// are, Starting if any is starting, else Stopped. Nothing running is
    /// Stopped.
    pub fn aggregate(states: impl IntoIterator<Item = ServiceState>) -> ServiceState {
        let states: Vec<ServiceState> = states.into_iter().collect();
        if states.contains(&ServiceState::Degraded) {
            ServiceState::Degraded
        } else if !states.is_empty() && states.iter().all(|s| *s == ServiceState::Healthy) {
            ServiceState::Healthy
        } else if states.contains(&ServiceState::Starting) {
            ServiceState::Starting
        } else {
            ServiceState::Stopped
        }
    }

    /// Inverse of `as_str`.
    pub fn parse(value: &str) -> Option<ServiceState> {
        match value {
            "stopped" => Some(ServiceState::Stopped),
            "starting" => Some(ServiceState::Starting),
            "healthy" => Some(ServiceState::Healthy),
            "degraded" => Some(ServiceState::Degraded),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_strings_round_trip() {
        for state in [
            ServiceState::Stopped,
            ServiceState::Starting,
            ServiceState::Healthy,
            ServiceState::Degraded,
        ] {
            assert_eq!(ServiceState::parse(state.as_str()), Some(state));
        }
        assert_eq!(ServiceState::Stopped.as_str(), "stopped");
        assert_eq!(ServiceState::Degraded.as_str(), "degraded");
        assert_eq!(ServiceState::parse("nonsense"), None);
    }

    #[test]
    fn aggregate_follows_the_status_light_rule() {
        use ServiceState::*;
        assert_eq!(ServiceState::aggregate([]), Stopped);
        assert_eq!(ServiceState::aggregate([Healthy, Healthy]), Healthy);
        assert_eq!(ServiceState::aggregate([Healthy, Starting]), Starting);
        assert_eq!(ServiceState::aggregate([Healthy, Stopped]), Stopped);
        assert_eq!(
            ServiceState::aggregate([Healthy, Starting, Degraded]),
            Degraded
        );
    }

    #[test]
    fn tiers_order_by_startup() {
        assert!(Tier::T0 < Tier::T1 && Tier::T1 < Tier::T2 && Tier::T2 < Tier::T3);
    }
}
