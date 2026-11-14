//! The two background tasks behind a running service: the crash monitor
//! (restart on the backoff schedule, trip the breaker) and the steady-state
//! probe (a hung service looks alive, so two failed probes kill it).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use capsule_core::ServiceState;
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::endpoint::Endpoint;
use crate::service::{ControlState, Shared, BREAKER_WINDOW};
use crate::should_trip_breaker;
use crate::spawn::{kill_pid, spawn_child};

/// Owns the child: reaps exits, distinguishes deliberate stops from crashes,
/// restarts on the backoff schedule, trips the breaker into Degraded.
pub(crate) async fn run_monitor(
    shared: Arc<Shared>,
    control: Arc<Mutex<ControlState>>,
    mut child: Child,
    generation: u64,
) {
    let mut attempt: u32 = 0;
    loop {
        let status = child.wait().await;
        if shared.generation.load(Ordering::SeqCst) != generation
            || shared.stopping.load(Ordering::SeqCst)
        {
            return; // deliberate stop — not a crash
        }
        shared.logs.append(&format!(
            "{} exited unexpectedly ({status:?})",
            shared.config.name
        ));

        let tripped = {
            let mut restarts = shared.restarts.lock().expect("restarts lock");
            let now = Instant::now();
            restarts.push_back(now);
            while let Some(front) = restarts.front() {
                if now.duration_since(*front) > BREAKER_WINDOW {
                    restarts.pop_front();
                } else {
                    break;
                }
            }
            should_trip_breaker(restarts.len() as u32)
        };
        if tripped {
            shared
                .logs
                .append("circuit breaker tripped: >5 restarts in 10 min — entering degraded");
            shared.set_state(ServiceState::Degraded);
            let mut ctl = control.lock().await;
            ctl.pid = None;
            shared.pgid.store(0, Ordering::SeqCst);
            return;
        }

        shared.set_state(ServiceState::Starting);
        tokio::time::sleep((shared.config.backoff)(attempt)).await;
        attempt = attempt.saturating_add(1);
        shared.note_restart();

        if shared.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let addr = {
            let ctl = control.lock().await;
            match ctl.addr.clone() {
                Some(addr) => addr,
                None => return,
            }
        };
        match spawn_child(&shared, &addr) {
            Ok((new_child, pid)) => {
                {
                    let mut ctl = control.lock().await;
                    ctl.pid = Some(pid);
                }
                shared.pgid.store(pid, Ordering::SeqCst);
                child = new_child;
            }
            Err(error) => {
                shared.logs.append(&format!("respawn failed: {error}"));
                shared.set_state(ServiceState::Degraded);
                return;
            }
        }

        let deadline = Instant::now() + shared.config.start_timeout;
        let mut ready = false;
        while Instant::now() < deadline {
            if shared
                .config
                .readiness
                .check(&addr, shared.config.probe_timeout)
                .await
            {
                ready = true;
                break;
            }
            if shared.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            if child.try_wait().ok().flatten().is_some() {
                break; // already dead — count it on the next wait()
            }
            tokio::time::sleep(shared.config.probe_interval).await;
        }
        if ready {
            shared.set_state(ServiceState::Healthy);
        } else if child.try_wait().ok().flatten().is_none() {
            let _ = child.start_kill();
        }
    }
}

/// Steady-state readiness probing (spec: every 30 s). Two consecutive failures
/// with the process still alive = hung service: kill it so the monitor's crash
/// path restarts it.
pub(crate) async fn run_steady_probe(
    shared: Arc<Shared>,
    control: Arc<Mutex<ControlState>>,
    addr: Endpoint,
    generation: u64,
) {
    let mut consecutive_failures: u32 = 0;
    loop {
        tokio::time::sleep(shared.config.steady_probe_interval).await;
        if shared.generation.load(Ordering::SeqCst) != generation
            || shared.stopping.load(Ordering::SeqCst)
        {
            return;
        }
        match *shared.state_tx.borrow() {
            ServiceState::Healthy => {}
            ServiceState::Starting => continue,
            ServiceState::Stopped | ServiceState::Degraded => return,
        }
        if shared
            .config
            .readiness
            .check(&addr, shared.config.probe_timeout)
            .await
        {
            consecutive_failures = 0;
        } else {
            consecutive_failures += 1;
            if consecutive_failures >= 2 {
                shared
                    .logs
                    .append("readiness failed twice while process alive — killing for restart");
                let pid = { control.lock().await.pid };
                if let Some(pid) = pid {
                    kill_pid(pid);
                }
                consecutive_failures = 0;
            }
        }
    }
}
