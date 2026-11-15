//! The real `System`: the supervisor's stack, the registry and the hardware
//! profile, presented in wire types.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use capsule_core::{HardwareSource, Profile, ServiceState, Tier};
use isonomic_proto as proto;
use registry::ServiceRegistry;
use supervisor::{ServiceSupervisor, Stack};
use tokio::sync::{broadcast, Notify};

use crate::System;

pub struct StackSystem {
    stack: Arc<Stack>,
    registry: Option<Arc<ServiceRegistry>>,
    hardware: Arc<dyn HardwareSource>,
    profile: Profile,
    cold_start_ms: OnceLock<u64>,
    changes: broadcast::Sender<proto::ServiceStatus>,
    shutdown: Notify,
}

fn wire_state(state: ServiceState) -> proto::ServiceState {
    match state {
        ServiceState::Stopped => proto::ServiceState::Stopped,
        ServiceState::Starting => proto::ServiceState::Starting,
        ServiceState::Healthy => proto::ServiceState::Healthy,
        ServiceState::Degraded => proto::ServiceState::Degraded,
    }
}

fn wire_tier(tier: Tier) -> u8 {
    match tier {
        Tier::T0 => 0,
        Tier::T1 => 1,
        Tier::T2 => 2,
        Tier::T3 => 3,
    }
}

impl StackSystem {
    /// Wrap a stack and start watching every service for changes. Must be
    /// called inside a Tokio runtime.
    pub fn new(
        stack: Arc<Stack>,
        registry: Option<Arc<ServiceRegistry>>,
        hardware: Arc<dyn HardwareSource>,
        profile: Profile,
    ) -> Arc<Self> {
        let (changes, _) = broadcast::channel(256);
        let system = Arc::new(Self {
            stack,
            registry,
            hardware,
            profile,
            cold_start_ms: OnceLock::new(),
            changes,
            shutdown: Notify::new(),
        });
        for (name, tier, supervisor) in system.stack.services() {
            let weak = Arc::downgrade(&system);
            let mut states = supervisor.subscribe();
            tokio::spawn(async move {
                // The supervisor writes the registry before it signals here,
                // so the row read below is never older than the change.
                while states.changed().await.is_ok() {
                    let Some(system) = weak.upgrade() else { return };
                    let _ = system
                        .changes
                        .send(system.describe(&name, tier, &supervisor));
                }
            });
        }
        system
    }

    /// Record how long the core services took to come up.
    pub fn set_cold_start(&self, elapsed: Duration) {
        let _ = self.cold_start_ms.set(elapsed.as_millis() as u64);
    }

    /// Resolves once a client has asked the engine to shut down.
    pub async fn shutdown_requested(&self) {
        self.shutdown.notified().await;
    }

    fn describe(
        &self,
        name: &str,
        tier: Tier,
        supervisor: &ServiceSupervisor,
    ) -> proto::ServiceStatus {
        let last_healthy_unix = self
            .registry
            .as_ref()
            .and_then(|registry| registry.get(name).ok().flatten())
            .and_then(|record| record.last_healthy_unix);
        proto::ServiceStatus {
            name: name.to_string(),
            tier: wire_tier(tier),
            state: wire_state(supervisor.state()),
            restart_count: supervisor.restart_total(),
            last_healthy_unix,
        }
    }
}

impl System for StackSystem {
    fn status(&self) -> proto::SystemStatus {
        // Read each service once, then derive the light from those same
        // readings, so the two can never disagree.
        // A lazy service that has not been started is idle, not down: it is
        // listed once it runs (the SDK's light counts every listed service).
        let members: Vec<_> = self
            .stack
            .services()
            .into_iter()
            .filter(|(_, tier, s)| !supervisor::stack::idle_lazy(*tier, s.state()))
            .collect();
        let states: Vec<ServiceState> = members.iter().map(|(_, _, s)| s.state()).collect();
        let services = members
            .iter()
            .zip(&states)
            .map(|((name, tier, supervisor), state)| proto::ServiceStatus {
                state: wire_state(*state),
                ..self.describe(name, *tier, supervisor)
            })
            .collect();
        proto::SystemStatus {
            aggregate: wire_state(ServiceState::aggregate(states)),
            services,
            cold_start_ms: self.cold_start_ms.get().copied(),
        }
    }

    fn changes(&self) -> broadcast::Receiver<proto::ServiceStatus> {
        self.changes.subscribe()
    }

    fn logs(&self, service: &str, limit: usize) -> Option<Vec<String>> {
        let lines = self.stack.get(service)?.last_logs();
        let skip = lines.len().saturating_sub(limit);
        Some(lines.into_iter().skip(skip).collect())
    }

    fn profile(&self) -> proto::HardwareProfile {
        let hardware = self.hardware.probe();
        proto::HardwareProfile {
            profile: self.profile.as_str().to_string(),
            total_memory_bytes: hardware.total_memory_bytes,
            available_memory_bytes: hardware.available_memory_bytes,
        }
    }

    fn request_shutdown(&self) {
        self.shutdown.notify_one();
    }
}
