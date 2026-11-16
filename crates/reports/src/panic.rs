//! The panic hook: before the engine's process ends, it writes a report
//! with the engine's last log lines. A panic in a task the runtime catches
//! is reported too: it is still a fault in the engine.
//!
//! [`frames`] and [`short_path`] are the window's (isonomic-desktop
//! `crates/reports/src/panic.rs`), copied so both backtraces read alike.
//! REPLACE WITH THE SDK'S SHARED CRATE with the scrubber.

use std::panic::PanicHookInfo;
use std::sync::Arc;

use crate::reporter::Reporter;
use crate::{Component, Kind, Report};

/// Backtraces are cut to this many frames.
pub const FRAMES: usize = 40;

/// Install the hook. The default hook still runs after it.
pub fn install(reporter: Arc<Reporter>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = reporter.save(from_panic(info));
        previous(info);
    }));
}

fn from_panic(info: &PanicHookInfo<'_>) -> Report {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".into());
    let at = info
        .location()
        .map(|l| format!(" at {}", short_path(&format!("{}:{}", l.file(), l.line()))))
        .unwrap_or_default();
    let mut report = Report::new(Kind::Panic, Component::Engine, format!("{message}{at}"));
    report.backtrace = frames(&std::backtrace::Backtrace::force_capture().to_string());
    report
}

/// The backtrace as function names and crate-relative locations.
pub fn frames(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(at) = line.strip_prefix("at ") {
            if let Some(last) = out.last_mut() {
                last.push_str(&format!(" ({})", short_path(at)));
            }
        } else if let Some((number, function)) = line.split_once(": ") {
            if number.chars().all(|c| c.is_ascii_digit()) {
                out.push(function.to_string());
            }
        }
        if out.len() >= FRAMES {
            break;
        }
    }
    out
}

/// A source path cut to its crate: `…/crates/broker/src/conn.rs:12:5`
/// becomes `broker/src/conn.rs:12`. Never a path on disk.
pub fn short_path(path: &str) -> String {
    let trimmed = match path.rsplit_once(':') {
        Some((head, col)) if col.chars().all(|c| c.is_ascii_digit()) && head.contains(':') => head,
        _ => path,
    };
    match trimmed.rfind("/src/") {
        Some(at) => {
            let before = &trimmed[..at];
            let krate = before.rsplit('/').next().unwrap_or(before);
            format!("{krate}{}", &trimmed[at..])
        }
        None => trimmed.rsplit('/').next().unwrap_or(trimmed).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::{log, Build, System};

    const TRACE: &str = "   0: std::backtrace::Backtrace::force_capture
             at /rustc/abc/library/std/src/backtrace.rs:312:13
   1: broker::conn::serve
             at /Users/omar/Code/isonomic-capsule/crates/broker/src/conn.rs:70:21";

    #[test]
    fn frames_keep_functions_and_crate_relative_places_only() {
        let frames = frames(TRACE);
        assert_eq!(
            frames,
            [
                "std::backtrace::Backtrace::force_capture (std/src/backtrace.rs:312)",
                "broker::conn::serve (broker/src/conn.rs:70)",
            ]
        );
    }

    #[test]
    fn the_hook_writes_a_report_with_the_engines_log_before_the_process_ends() {
        let capsule = tempfile::tempdir().unwrap();
        let home = capsule.path().to_str().unwrap().to_string();
        let reporter = Arc::new(Reporter::new(
            Store::in_capsule(capsule.path()),
            Build::default(),
            System::default(),
            Some(home.clone()),
        ));
        log::remember(&format!("capsule directory: {home}/Capsule"));
        log::remember("cold start: 812 ms");
        install(Arc::clone(&reporter));
        let caught = std::panic::catch_unwind(|| panic!("the engine fell over"));
        let _ = std::panic::take_hook();
        assert!(caught.is_err());
        let pending = reporter.store.pending();
        assert_eq!(pending.len(), 1);
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&pending[0]).unwrap()).unwrap();
        assert_eq!(doc["kind"], "panic");
        assert_eq!(doc["component"]["id"], "engine");
        let message = doc["message"].as_str().unwrap();
        assert!(
            message.starts_with("the engine fell over at reports/src/panic.rs:"),
            "{message}"
        );
        assert!(doc["backtrace"].as_array().is_some_and(|b| !b.is_empty()));
        let tail = doc["log_tail"].as_array().unwrap();
        assert!(
            tail.iter().any(|l| l == "capsule directory: ~/Capsule"),
            "{tail:?}"
        );
        assert!(!doc.to_string().contains(&home));
    }
}
