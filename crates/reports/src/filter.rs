//! Which of the shared log rules (`isonomic_reports::filter`) each of the
//! engine's services follows, and the model server's allowed prefixes:
//!
//! - **The database:** its severities and first sentences only.
//! - **The model server:** only lines that start with a prefix on
//!   [`MODEL_PREFIXES`]: loading, the server's start, and its errors.
//!   Request and slot lines are dropped.
//! - **Every other service:** every line, with quoted text removed and cut.

use isonomic_reports::filter::{allow_listed, any_log, database_log};
pub use isonomic_reports::filter::{last_exit, LINE_LIMIT};

/// The model server lines a report keeps, by how they start.
pub const MODEL_PREFIXES: &[&str] = &[
    "main:",
    "build:",
    "system info:",
    "error:",
    "llama_model_load",
    "llama_init_from_model:",
    "llama_context:",
    "load_tensors:",
    "common_init_from_params:",
    "srv    load_model:",
    "srv  load_model:",
    "ggml_",
    "GGML_ASSERT",
    "terminate called",
    "libc++abi:",
];

/// Which log rules a service's lines follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Database,
    ModelServer,
    Other,
}

impl Source {
    /// The rules for the service the supervisor calls `name`.
    pub fn of(name: &str) -> Self {
        match name {
            "postgres" => Source::Database,
            "llama-server" => Source::ModelServer,
            _ => Source::Other,
        }
    }
}

/// The lines of `source`'s log a report may keep, oldest first.
pub fn service_log(source: Source, lines: &[String]) -> Vec<String> {
    match source {
        Source::Database => database_log(lines),
        Source::ModelServer => allow_listed(lines, MODEL_PREFIXES),
        Source::Other => any_log(lines),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    #[test]
    fn each_service_follows_its_own_rules() {
        assert_eq!(Source::of("postgres"), Source::Database);
        assert_eq!(Source::of("llama-server"), Source::ModelServer);
        assert_eq!(Source::of("cache"), Source::Other);
        let database = lines(
            "[stderr] 2026-11-16 10:00:01.004 UTC [907] ERROR:  duplicate key value violates unique constraint \"trips_pkey\"
[stderr] 2026-11-16 10:00:01.004 UTC [907] STATEMENT:  insert into trips (title)
[stderr] \tvalues ('Lisbon with my sister')",
        );
        assert_eq!(
            service_log(Source::Database, &database),
            ["ERROR: duplicate key value violates unique constraint \"[text removed]\""]
        );
        let model = lines(
            "[stderr] main: server is listening on http://127.0.0.1:41000
[stderr] slot update_slots: id  0 | task 3 | prompt: Plan one day in Lisbon",
        );
        assert_eq!(
            service_log(Source::ModelServer, &model),
            ["main: server is listening on http://127.0.0.1:41000"]
        );
        assert_eq!(
            service_log(
                Source::Other,
                &lines("[stdout] Ready to accept connections")
            ),
            ["Ready to accept connections"]
        );
    }
}
