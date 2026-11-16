//! What every report from this engine shares: where it goes, the build and
//! machine it names, and the home folder it is scrubbed against.
//!
//! **A run of identical panics is one report.** An engine that panics at
//! every start is relaunched by the login agent each time; rather than 20
//! identical reports, a panic with the same message and component as a
//! pending panic report from the last [`PANIC_RUN`] updates that report:
//! its `repeats` counts the panics after the first, and `when.last_unix_secs`
//! says when the latest was. Its `id` stays, since it is the same report.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::store::Store;
use crate::{log, Build, Kind, Report, System};

/// Panics this close together, with the same message, are one report.
pub const PANIC_RUN: Duration = Duration::from_secs(10 * 60);

/// Milliseconds since the Unix epoch; injectable so tests move time.
pub type Now = Box<dyn Fn() -> u128 + Send + Sync>;

pub struct Reporter {
    pub store: Store,
    pub build: Build,
    pub system: Mutex<System>,
    pub home: Option<String>,
    pub started: Instant,
    now: Now,
}

fn system_now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

impl Reporter {
    pub fn new(store: Store, build: Build, system: System, home: Option<String>) -> Self {
        Self {
            store,
            build,
            system: Mutex::new(system),
            home,
            started: Instant::now(),
            now: Box::new(system_now),
        }
    }

    /// The same reporter on another clock.
    pub fn with_now(mut self, now: Now) -> Self {
        self.now = now;
        self
    }

    /// Write `report` with this engine's build and machine. A panic report
    /// carries the engine's own log; a service's carries its filtered log,
    /// set by the caller.
    pub fn save(&self, mut report: Report) -> std::io::Result<PathBuf> {
        if report.kind == Kind::Panic && report.log_tail.is_empty() {
            report.log_tail = log::tail();
        }
        report.uptime_secs = self.started.elapsed().as_secs();
        let now = (self.now)();
        let system = match self.system.lock() {
            Ok(s) => s.clone(),
            Err(p) => p.into_inner().clone(),
        };
        let unix_secs = (now / 1000) as u64;
        let doc = report.to_json(&self.build, &system, self.home.as_deref(), unix_secs);
        if report.kind == Kind::Panic {
            if let Some(path) = self.repeat(&doc, unix_secs)? {
                return Ok(path);
            }
        }
        self.store.save(&doc, now)
    }

    /// Count `doc` on a pending panic report it repeats, if there is one.
    fn repeat(&self, doc: &Value, unix_secs: u64) -> std::io::Result<Option<PathBuf>> {
        let run = PANIC_RUN.as_secs();
        for path in self.store.pending().into_iter().rev() {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(mut kept) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let latest = kept["when"]["last_unix_secs"]
                .as_u64()
                .or_else(|| kept["when"]["unix_secs"].as_u64())
                .unwrap_or(0);
            let same = kept["kind"] == doc["kind"]
                && kept["message"] == doc["message"]
                && kept["component"]["id"] == doc["component"]["id"];
            if !same || unix_secs.saturating_sub(latest) >= run {
                continue;
            }
            let repeats = kept["repeats"].as_u64().unwrap_or(0) + 1;
            kept["repeats"] = json!(repeats);
            kept["when"]["last_unix_secs"] = json!(unix_secs);
            self.store.replace(&path, &kept)?;
            return Ok(Some(path));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::Component;

    fn reporter(dir: &std::path::Path, clock: Arc<AtomicU64>) -> Reporter {
        Reporter::new(
            Store::in_capsule(dir),
            Build::default(),
            System::default(),
            None,
        )
        .with_now(Box::new(move || u128::from(clock.load(Ordering::SeqCst))))
    }

    fn panic(message: &str) -> Report {
        Report::new(Kind::Panic, Component::Engine, message)
    }

    #[test]
    fn a_run_of_identical_panics_is_one_report_with_a_count() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(AtomicU64::new(1_800_000_000_000));
        let reporter = reporter(dir.path(), Arc::clone(&clock));
        let first = reporter
            .save(panic("fell over at broker/src/conn.rs:70"))
            .unwrap();
        for _ in 0..4 {
            // A relaunch every two minutes keeps the run going.
            clock.fetch_add(120_000, Ordering::SeqCst);
            let again = reporter
                .save(panic("fell over at broker/src/conn.rs:70"))
                .unwrap();
            assert_eq!(again, first);
        }
        let pending = reporter.store.pending();
        assert_eq!(pending.len(), 1);
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&first).unwrap()).unwrap();
        assert_eq!(doc["repeats"], 4);
        assert_eq!(doc["when"]["last_unix_secs"], 1_800_000_480);
        assert_eq!(doc["when"]["unix_secs"], 1_800_000_000, "the first stays");

        // Another panic is its own report; so is the same one after a pause.
        reporter.save(panic("something else")).unwrap();
        clock.fetch_add(PANIC_RUN.as_millis() as u64 + 1, Ordering::SeqCst);
        reporter
            .save(panic("fell over at broker/src/conn.rs:70"))
            .unwrap();
        assert_eq!(reporter.store.pending().len(), 3);
    }

    #[test]
    fn other_kinds_are_never_merged() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(AtomicU64::new(1_800_000_000_000));
        let reporter = reporter(dir.path(), clock);
        for _ in 0..2 {
            let service = Component::Service {
                name: "cache".into(),
                version: "9.1.1".into(),
                restarts: 6,
                last_exit: None,
            };
            reporter
                .save(Report::new(Kind::ServiceFailed, service, "cache quit"))
                .unwrap();
        }
        assert_eq!(reporter.store.pending().len(), 2);
    }
}
