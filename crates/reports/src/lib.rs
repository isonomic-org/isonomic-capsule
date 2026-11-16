//! Problem reports from the engine, in the window's format 1.
//!
//! A report is one small JSON document written to the Capsule's
//! `reports/pending/`, where the window keeps, shows and prunes its own. The
//! engine writes three kinds the window does not: a panic in the engine,
//! `service_failed` when a bundled service's breaker trips, and
//! `model_server_failed` when a model worker's does. Nothing here sends.
//!
//! - [`scrub`]: what never goes in a report. A copy of the window's rules,
//!   to be replaced by the SDK's shared crate when it has one.
//! - [`filter`]: the harder rules for service logs.
//! - [`store`]: writing a report beside the window's; [`Reporter`]: what
//!   every report shares.
//! - [`log`]: the engine's own log, whose last lines a report carries.
//! - [`panic`]: the panic hook. [`watch`]: services and model workers.

pub mod filter;
pub mod log;
pub mod panic;
pub mod reporter;
pub mod scrub;
pub mod store;
pub mod system;
pub mod watch;

pub use reporter::Reporter;

use serde_json::{json, Map, Value};

/// The report format, the window's.
pub const FORMAT: u32 = 1;
/// How many log lines a report carries.
pub const LOG_LINES: usize = 50;

/// What a report is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The engine panicked.
    Panic,
    /// A bundled service failed too often and was given up on.
    ServiceFailed,
    /// A model worker failed too often and was given up on.
    ModelServerFailed,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Panic => "panic",
            Kind::ServiceFailed => "service_failed",
            Kind::ModelServerFailed => "model_server_failed",
        }
    }
}

/// The build a report comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Build {
    pub version: String,
    pub build: String,
    pub arch: String,
    /// The SDK, Chat and engine commits, as far as the engine knows them.
    pub sdk: String,
    pub chat: String,
    pub engine: String,
}

/// The machine a report comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    pub macos: String,
    pub profile: Option<String>,
    pub memory_gb: Option<u64>,
    pub capsule_format: Option<u32>,
}

/// Which part of the engine failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Component {
    /// The engine itself: `engine`.
    Engine,
    /// A bundled service: `engine/<name>`.
    Service {
        name: String,
        version: String,
        restarts: u32,
        last_exit: Option<String>,
    },
    /// A model worker: `engine/<runtime>`, with the model it ran.
    ModelServer {
        model: String,
        runtime: String,
        version: String,
        restarts: u32,
        last_exit: Option<String>,
    },
}

/// One report, before it is written.
#[derive(Debug, Clone)]
pub struct Report {
    pub kind: Kind,
    pub component: Component,
    pub message: String,
    pub backtrace: Vec<String>,
    /// Already filtered for its source; scrubbed again when written.
    pub log_tail: Vec<String>,
    pub uptime_secs: u64,
}

impl Report {
    pub fn new(kind: Kind, component: Component, message: impl Into<String>) -> Self {
        Self {
            kind,
            component,
            message: message.into(),
            backtrace: Vec::new(),
            log_tail: Vec::new(),
            uptime_secs: 0,
        }
    }

    /// The document as written: every text field scrubbed against `home`.
    /// The window's fields, plus `service` or `model` for the engine's kinds.
    pub fn to_json(
        &self,
        build: &Build,
        system: &System,
        home: Option<&str>,
        unix_secs: u64,
    ) -> Value {
        let line = |l: &String| scrub::line(l, home);
        let exit = |e: &Option<String>| e.as_ref().map(line);
        let mut extra = Map::new();
        let component = match &self.component {
            Component::Engine => json!({ "id": "engine", "version": build.version }),
            Component::Service {
                name,
                version,
                restarts,
                last_exit,
            } => {
                extra.insert(
                    "service".into(),
                    json!({ "name": name, "version": version, "restarts": restarts,
                            "last_exit": exit(last_exit) }),
                );
                json!({ "id": format!("engine/{name}"), "version": version })
            }
            Component::ModelServer {
                model,
                runtime,
                version,
                restarts,
                last_exit,
            } => {
                extra.insert(
                    "model".into(),
                    json!({ "id": model, "runtime": runtime, "restarts": restarts,
                            "last_exit": exit(last_exit) }),
                );
                json!({ "id": format!("engine/{runtime}"), "version": version })
            }
        };
        let mut doc = json!({
            "format": FORMAT,
            "kind": self.kind.as_str(),
            "component": component,
            "product": {
                "version": build.version,
                "build": build.build,
                "arch": build.arch,
                "pins": { "sdk": build.sdk, "chat": build.chat, "engine": build.engine },
            },
            "system": {
                "macos": system.macos,
                "profile": system.profile,
                "memory_gb": system.memory_gb,
                "capsule_format": system.capsule_format,
            },
            "when": { "unix_secs": unix_secs, "uptime_secs": self.uptime_secs },
            "message": scrub::message(&self.message, home),
            "backtrace": self.backtrace.iter().map(line).collect::<Vec<_>>(),
            "log_tail": self.log_tail.iter().map(line).collect::<Vec<_>>(),
            "tab": Value::Null,
        });
        if let Value::Object(map) = &mut doc {
            map.extend(extra);
        }
        doc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_report_names_the_service_and_keeps_the_windows_fields() {
        let build = Build {
            version: "0.9.0".into(),
            engine: "506097d".into(),
            ..Default::default()
        };
        let system = System {
            macos: "15.1".into(),
            profile: Some("standard".into()),
            memory_gb: Some(16),
            capsule_format: Some(1),
        };
        let report = Report::new(
            Kind::ServiceFailed,
            Component::Service {
                name: "postgres".into(),
                version: "18.4".into(),
                restarts: 6,
                last_exit: Some("exit code 1".into()),
            },
            "postgres stopped at /Users/omar/Library/x",
        );
        let doc = report.to_json(&build, &system, Some("/Users/omar"), 1_700_000_000);
        assert_eq!(doc["format"], FORMAT);
        assert_eq!(doc["kind"], "service_failed");
        assert_eq!(doc["component"]["id"], "engine/postgres");
        assert_eq!(doc["service"]["restarts"], 6);
        assert_eq!(doc["service"]["last_exit"], "exit code 1");
        assert_eq!(doc["product"]["pins"]["engine"], "506097d");
        assert_eq!(doc["system"]["capsule_format"], 1);
        assert_eq!(doc["message"], "postgres stopped at ~/Library/x");
        assert!(doc["tab"].is_null());
    }

    #[test]
    fn a_model_report_names_the_model_and_its_runtime() {
        let report = Report::new(
            Kind::ModelServerFailed,
            Component::ModelServer {
                model: "qwen3.5-4b".into(),
                runtime: "llama-server".into(),
                version: "b12387".into(),
                restarts: 6,
                last_exit: None,
            },
            "the model server stopped",
        );
        let doc = report.to_json(&Build::default(), &System::default(), None, 0);
        assert_eq!(doc["component"]["id"], "engine/llama-server");
        assert_eq!(doc["model"]["id"], "qwen3.5-4b");
        assert!(doc["model"]["last_exit"].is_null());
    }
}
