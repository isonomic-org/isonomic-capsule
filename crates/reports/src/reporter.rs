//! What every report from this engine shares: where it goes, the build and
//! machine it names, and the home folder it is scrubbed against.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::store::Store;
use crate::{log, Build, Kind, Report, System};

pub struct Reporter {
    pub store: Store,
    pub build: Build,
    pub system: Mutex<System>,
    pub home: Option<String>,
    pub started: Instant,
}

impl Reporter {
    pub fn new(store: Store, build: Build, system: System, home: Option<String>) -> Self {
        Self {
            store,
            build,
            system: Mutex::new(system),
            home,
            started: Instant::now(),
        }
    }

    /// Write `report` with this engine's build and machine. A panic report
    /// carries the engine's own log; a service's carries its filtered log,
    /// set by the caller.
    pub fn save(&self, mut report: Report) -> std::io::Result<PathBuf> {
        if report.kind == Kind::Panic && report.log_tail.is_empty() {
            report.log_tail = log::tail();
        }
        report.uptime_secs = self.started.elapsed().as_secs();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let system = match self.system.lock() {
            Ok(s) => s.clone(),
            Err(p) => p.into_inner().clone(),
        };
        let doc = report.to_json(&self.build, &system, self.home.as_deref(), now.as_secs());
        self.store.save(&doc, now.as_millis())
    }
}
