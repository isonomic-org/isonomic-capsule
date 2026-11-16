//! Watching what the engine runs, from outside its supervisors: their state
//! is published on a watch, so nothing here changes how they behave.
//!
//! - A bundled service's breaker trips (Degraded): one `service_failed`.
//! - A model worker quits unexpectedly and is restarted: one
//!   `model_server_failed` for its first such crash. Its breaker trips: one
//!   more. A deliberate stop or an idle unload is not a failure.

use std::sync::Arc;

use isonomic_llama::LlamaSupervisor;
use supervisor::{ServiceState, ServiceSupervisor};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::filter::{last_exit, service_log, Source};
use crate::reporter::Reporter;
use crate::{log, Component, Kind, Report};

fn save(reporter: &Reporter, report: Report) {
    let what = format!("{} report", report.kind.as_str());
    match reporter.save(report) {
        Ok(_) => log::line(&format!("wrote a {what} for the window")),
        Err(e) => log::line(&format!("could not write a {what}: {e}")),
    }
}

/// Report `service` each time its breaker trips. `version` is its bundled
/// version, from the sidecar manifest.
pub fn service(
    reporter: Arc<Reporter>,
    service: Arc<ServiceSupervisor>,
    version: String,
) -> JoinHandle<()> {
    let mut rx = service.subscribe();
    tokio::spawn(async move {
        let mut was = *rx.borrow_and_update();
        while rx.changed().await.is_ok() {
            let now = *rx.borrow_and_update();
            if now == ServiceState::Degraded && was != ServiceState::Degraded {
                let logs = service.last_logs();
                let name = service.name().to_string();
                let restarts = service.restart_total();
                let mut report = Report::new(
                    Kind::ServiceFailed,
                    Component::Service {
                        name: name.clone(),
                        version: version.clone(),
                        restarts,
                        last_exit: last_exit(&logs),
                    },
                    format!("{name} quit too often and was stopped after {restarts} restarts"),
                );
                report.log_tail = service_log(Source::of(&name), &logs);
                save(&reporter, report);
            }
            was = now;
        }
    })
}

/// The worker states a model watch follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Crashed,
    Degraded,
    Other,
}

/// What a change from `was` to `now` means, given the restart counts
/// before and after: a change can be missed, so a higher count is a crash.
fn seen(
    was: isonomic_llama::ServiceState,
    now: isonomic_llama::ServiceState,
    restarts_before: u32,
    restarts: u32,
) -> Seen {
    use isonomic_llama::ServiceState as S;
    match now {
        S::Degraded if was != S::Degraded => Seen::Degraded,
        S::Degraded => Seen::Other,
        S::Starting if was == S::Healthy => Seen::Crashed,
        _ if restarts > restarts_before => Seen::Crashed,
        _ => Seen::Other,
    }
}

/// Report `model`'s worker on its first crash and when its breaker trips.
/// `runtime` and `version` name the bundled server.
pub fn model_server(
    reporter: Arc<Reporter>,
    model: String,
    worker: Arc<LlamaSupervisor>,
    runtime: String,
    version: String,
) -> JoinHandle<()> {
    let mut rx: watch::Receiver<_> = worker.subscribe();
    tokio::spawn(async move {
        let mut was = *rx.borrow_and_update();
        let mut restarts_seen = worker.restart_total();
        let mut crash_reported = false;
        while rx.changed().await.is_ok() {
            let now = *rx.borrow_and_update();
            let restarts = worker.restart_total();
            let message = match seen(was, now, restarts_seen, restarts) {
                Seen::Crashed if !crash_reported => {
                    crash_reported = true;
                    format!("the model server for {model} quit unexpectedly and was restarted")
                }
                Seen::Degraded => format!(
                    "the model server for {model} quit too often and was stopped after {restarts} restarts"
                ),
                _ => {
                    was = now;
                    restarts_seen = restarts;
                    continue;
                }
            };
            let logs = worker.last_logs();
            let mut report = Report::new(
                Kind::ModelServerFailed,
                Component::ModelServer {
                    model: model.clone(),
                    runtime: runtime.clone(),
                    version: version.clone(),
                    restarts,
                    last_exit: last_exit(&logs),
                },
                message,
            );
            report.log_tail = service_log(Source::ModelServer, &logs);
            save(&reporter, report);
            was = now;
            restarts_seen = restarts;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use isonomic_llama::ServiceState as S;

    #[test]
    fn a_restart_is_a_crash_and_a_stop_is_not() {
        assert_eq!(seen(S::Healthy, S::Starting, 0, 0), Seen::Crashed);
        assert_eq!(seen(S::Starting, S::Healthy, 0, 1), Seen::Crashed);
        assert_eq!(seen(S::Healthy, S::Stopped, 0, 0), Seen::Other);
        assert_eq!(seen(S::Stopped, S::Starting, 1, 1), Seen::Other);
        assert_eq!(seen(S::Starting, S::Degraded, 5, 5), Seen::Degraded);
        assert_eq!(seen(S::Degraded, S::Degraded, 5, 5), Seen::Other);
    }
}
