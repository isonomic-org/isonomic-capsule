//! Startup stack (architecture spec, section 4.2): tier 0 first, tier 1 in
//! parallel once tier 0 is healthy. Records cold-start time, aggregates state
//! for the status light, and stops everything in reverse dependency order.
//!
//! Service definitions live next to the service (`postgres.rs`); binaries and
//! data directories are plain inputs from the engine.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use capsule_core::{ServiceState, Tier};
use registry::ServiceRegistry;

use crate::service::{ServiceConfig, ServiceError, ServiceSupervisor};

/// One service's placement in the stack.
pub struct StackService {
    pub tier: Tier,
    pub supervisor: Arc<ServiceSupervisor>,
}

/// The service stack.
pub struct Stack {
    services: BTreeMap<String, StackService>,
    order: Vec<String>,
    registry: Option<Arc<ServiceRegistry>>,
}

impl Stack {
    pub fn new(registry: Option<Arc<ServiceRegistry>>) -> Self {
        Self {
            services: BTreeMap::new(),
            order: Vec::new(),
            registry,
        }
    }

    /// Register a service at a tier. Insertion order within a tier is the
    /// startup order; reverse of overall insertion is the shutdown order.
    pub fn add(
        &mut self,
        name: impl Into<String>,
        tier: Tier,
        config: ServiceConfig,
    ) -> Arc<ServiceSupervisor> {
        let name = name.into();
        let supervisor = Arc::new(ServiceSupervisor::new(config, self.registry.clone()));
        self.services.insert(
            name.clone(),
            StackService {
                tier,
                supervisor: Arc::clone(&supervisor),
            },
        );
        self.order.push(name);
        supervisor
    }

    pub fn get(&self, name: &str) -> Option<&Arc<ServiceSupervisor>> {
        self.services.get(name).map(|s| &s.supervisor)
    }

    /// Every service with its tier, in registration order.
    pub fn services(&self) -> Vec<(String, Tier, Arc<ServiceSupervisor>)> {
        self.order
            .iter()
            .filter_map(|name| {
                self.services
                    .get(name)
                    .map(|svc| (name.clone(), svc.tier, Arc::clone(&svc.supervisor)))
            })
            .collect()
    }

    /// Every service's current state, by name, in registration order.
    pub fn states(&self) -> Vec<(String, ServiceState)> {
        self.order
            .iter()
            .filter_map(|name| {
                self.services
                    .get(name)
                    .map(|svc| (name.clone(), svc.supervisor.state()))
            })
            .collect()
    }

    fn tier_members(&self, tier: Tier) -> Vec<Arc<ServiceSupervisor>> {
        self.order
            .iter()
            .filter_map(|name| self.services.get(name))
            .filter(|s| s.tier == tier)
            .map(|s| Arc::clone(&s.supervisor))
            .collect()
    }

    /// Start tier 0, wait for it healthy, then start tier 1 in parallel and
    /// wait for all of it. Returns the cold-start elapsed time. Tier 2 is lazy
    /// and not started here.
    pub async fn start_tier_0_1(&self) -> Result<Duration, ServiceError> {
        let started = Instant::now();
        // Tier 0: sequential. The in-process parts need no supervision, so this
        // is usually empty.
        for svc in self.tier_members(Tier::T0) {
            svc.start().await?;
        }
        // Tier 1: parallel.
        let mut handles = Vec::new();
        for svc in self.tier_members(Tier::T1) {
            handles.push(tokio::spawn(async move { svc.start().await.map(|_| ()) }));
        }
        for handle in handles {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                Err(join) => return Err(ServiceError::Io(std::io::Error::other(join.to_string()))),
            }
        }
        Ok(started.elapsed())
    }

    /// Aggregate status light: Degraded if any
    /// service is degraded, else Healthy only if every service is healthy, else
    /// Starting if any is starting, else Stopped.
    pub fn aggregate_state(&self) -> ServiceState {
        let states: Vec<ServiceState> = self
            .services
            .values()
            .map(|s| s.supervisor.state())
            .collect();
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

    /// Graceful shutdown in reverse dependency order — app processes (tier 3)
    /// first, then lazy tier 2, then tier 1, then tier 0; reverse insertion
    /// order within a tier. Each service gets its configured stop signal
    /// (SIGINT for Postgres, the `pg_ctl stop -m fast` equivalent) and its
    /// stop grace before the process group is killed. This is what Quit
    /// runs.
    pub async fn shutdown(&self) {
        for tier in [Tier::T3, Tier::T2, Tier::T1, Tier::T0] {
            let mut members = self.tier_members(tier);
            members.reverse();
            for svc in members {
                svc.stop().await;
            }
        }
    }
}

#[cfg(all(test, unix))]
#[path = "stack_tests.rs"]
mod tests;
