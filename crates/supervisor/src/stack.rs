// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Startup stack (architecture spec, section 4.2): tier 0 first, tier 1 in
//! parallel once tier 0 is healthy. Records cold-start time, aggregates state
//! for the status light, and stops everything in reverse dependency order.
//!
//! Service definitions live next to the service (`postgres.rs`); binaries and
//! data directories are plain inputs from the engine.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use capsule_core::{RamDenial, RamGate, ServiceState, Tier};
use registry::ServiceRegistry;

use crate::service::{ServiceConfig, ServiceError, ServiceSupervisor};

/// Why a lazy service could not be started. Shared by every caller that
/// waited on the same attempt.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LazyError {
    #[error("no service named {0}")]
    Unknown(String),
    #[error("{0} is not on the lazy tier")]
    NotLazy(String),
    #[error("{name} did not start: {reason}")]
    Failed { name: String, reason: String },
    /// The RAM gate refused it; nothing was started.
    #[error("{name} was not started: {denial}")]
    Refused { name: String, denial: RamDenial },
}

/// One lazy service's start, shared: callers queue on the lock, and a caller
/// that waited through someone else's attempt takes that attempt's outcome.
#[derive(Default)]
struct LazyStart {
    lock: tokio::sync::Mutex<()>,
    attempts: AtomicU64,
    last: StdMutex<Option<Result<(), LazyError>>>,
}

/// One service's placement in the stack.
pub struct StackService {
    pub tier: Tier,
    pub supervisor: Arc<ServiceSupervisor>,
    /// Bytes the RAM gate must admit before each start. Only tier-2 services
    /// carry one.
    pub memory_bytes: Option<u64>,
    lazy: Arc<LazyStart>,
}

/// The service stack.
pub struct Stack {
    services: BTreeMap<String, StackService>,
    order: Vec<String>,
    registry: Option<Arc<ServiceRegistry>>,
    gate: Option<Arc<dyn RamGate>>,
}

impl Stack {
    pub fn new(registry: Option<Arc<ServiceRegistry>>) -> Self {
        Self {
            services: BTreeMap::new(),
            order: Vec::new(),
            registry,
            gate: None,
        }
    }

    /// The RAM gate consulted before starting a service added with
    /// `add_gated`. The engine passes the profile gate; tests pass their own.
    pub fn with_gate(mut self, gate: Arc<dyn RamGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// Register a service at a tier. Insertion order within a tier is the
    /// startup order; reverse of overall insertion is the shutdown order.
    pub fn add(
        &mut self,
        name: impl Into<String>,
        tier: Tier,
        config: ServiceConfig,
    ) -> Arc<ServiceSupervisor> {
        self.insert(name.into(), tier, config, None)
    }

    /// Register a lazy service that the RAM gate must admit, at
    /// `memory_bytes`, before every start.
    pub fn add_gated(
        &mut self,
        name: impl Into<String>,
        config: ServiceConfig,
        memory_bytes: u64,
    ) -> Arc<ServiceSupervisor> {
        self.insert(name.into(), Tier::T2, config, Some(memory_bytes))
    }

    fn insert(
        &mut self,
        name: String,
        tier: Tier,
        config: ServiceConfig,
        memory_bytes: Option<u64>,
    ) -> Arc<ServiceSupervisor> {
        let supervisor = Arc::new(ServiceSupervisor::new(config, self.registry.clone()));
        self.services.insert(
            name.clone(),
            StackService {
                tier,
                supervisor: Arc::clone(&supervisor),
                memory_bytes,
                lazy: Arc::default(),
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

    /// Start a tier-2 service on first use and wait for it to be healthy.
    /// Concurrent callers share one start; a failure is reported to every
    /// caller that waited on it, and the next call tries again. A gated
    /// service is checked against the RAM gate before each start, and a
    /// refusal starts nothing.
    pub async fn ensure_started(&self, name: &str) -> Result<(), LazyError> {
        let entry = self
            .services
            .get(name)
            .ok_or_else(|| LazyError::Unknown(name.to_string()))?;
        if entry.tier != Tier::T2 {
            return Err(LazyError::NotLazy(name.to_string()));
        }
        let lazy = &entry.lazy;
        let seen = lazy.attempts.load(Ordering::SeqCst);
        let _turn = lazy.lock.lock().await;
        if entry.supervisor.state() == ServiceState::Healthy {
            return Ok(());
        }
        if lazy.attempts.load(Ordering::SeqCst) != seen {
            // An attempt finished while this caller waited: share its outcome.
            if let Some(outcome) = lazy.last.lock().expect("lazy outcome").clone() {
                return outcome;
            }
        }
        let outcome = match self.admit(name, entry) {
            Err(refused) => Err(refused),
            Ok(()) => {
                entry
                    .supervisor
                    .start()
                    .await
                    .map(|_| ())
                    .map_err(|error| LazyError::Failed {
                        name: name.to_string(),
                        reason: error.to_string(),
                    })
            }
        };
        *lazy.last.lock().expect("lazy outcome") = Some(outcome.clone());
        lazy.attempts.fetch_add(1, Ordering::SeqCst);
        outcome
    }

    /// The RAM gate's verdict for a gated service. A gated service without a
    /// gate is refused to start rather than started unchecked.
    fn admit(&self, name: &str, entry: &StackService) -> Result<(), LazyError> {
        let Some(bytes) = entry.memory_bytes else {
            return Ok(());
        };
        let Some(gate) = &self.gate else {
            return Err(LazyError::Failed {
                name: name.to_string(),
                reason: "it needs the RAM gate and none is configured".to_string(),
            });
        };
        gate.admit(bytes).map_err(|denial| LazyError::Refused {
            name: name.to_string(),
            denial,
        })
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

/// A tier-2 service that is not running is idle, not down. The broker leaves
/// it out of `system.status` (and so out of the window's light) until it is
/// first started. `Stack::aggregate_state` keeps its ported behaviour.
pub fn idle_lazy(tier: Tier, state: ServiceState) -> bool {
    tier == Tier::T2 && state == ServiceState::Stopped
}

#[cfg(all(test, unix))]
#[path = "stack_tests.rs"]
mod tests;
