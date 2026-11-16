//! The harder rules for a service's log, before it goes in a report.
//!
//! A service's log can carry what a person made: a statement's values, a
//! prompt. So a report keeps only what each service says about itself:
//!
//! - **The database:** only `LOG`, `WARNING`, `ERROR`, `FATAL` and `PANIC`
//!   lines, as their severity and first sentence. `STATEMENT`, `DETAIL`,
//!   `CONTEXT`, `HINT` and `QUERY` lines, and every line that continues
//!   one, are dropped.
//! - **The model server:** only lines that start with a prefix on
//!   [`MODEL_PREFIXES`]: loading, the server's start, and its errors.
//!   Request and slot lines are dropped.
//! - **Every service:** quoted text of any length is removed, and each line
//!   is cut at [`LINE_LIMIT`] characters.
//!
//! The supervisor's own lines (a start, an exit, the breaker) carry no tag
//! and are kept. Every line is scrubbed again when the report is written.

use crate::scrub::{cut, quoted};

/// A kept log line is cut at this many characters.
pub const LINE_LIMIT: usize = 200;

/// What the database says that a report keeps.
const DATABASE_KEPT: &[&str] = &["LOG", "WARNING", "ERROR", "FATAL", "PANIC"];

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

/// A line the service printed, without the supervisor's `[stdout] ` or
/// `[stderr] ` tag; `None` for the supervisor's own lines.
fn printed(line: &str) -> Option<&str> {
    line.strip_prefix("[stdout] ")
        .or_else(|| line.strip_prefix("[stderr] "))
}

/// The lines of `source`'s log a report may keep, oldest first.
pub fn service_log(source: Source, lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| match printed(line) {
            None => Some(line.clone()),
            Some(text) => match source {
                Source::Database => database(text),
                Source::ModelServer => model_server(text),
                Source::Other => Some(text.to_string()),
            },
        })
        .map(|line| cut(&quoted(&line, 0), LINE_LIMIT))
        .collect()
}

/// A database line as its severity and first sentence, if it is kept.
/// Lines without a severity continue a statement and are dropped.
fn database(text: &str) -> Option<String> {
    let (head, rest) = text.split_once(":  ")?;
    let severity = head.rsplit(' ').next()?;
    if !DATABASE_KEPT.contains(&severity) {
        return None;
    }
    let sentence = match rest.find(". ") {
        Some(end) => &rest[..=end],
        None => rest,
    };
    Some(format!("{severity}: {}", sentence.trim_end()))
}

fn model_server(text: &str) -> Option<String> {
    MODEL_PREFIXES
        .iter()
        .any(|p| text.starts_with(p))
        .then(|| text.to_string())
}

/// How the service last ended, from the supervisor's own line for it:
/// `exit code 1`, `signal 9`, else the status as written.
pub fn last_exit(lines: &[String]) -> Option<String> {
    let line = lines
        .iter()
        .rev()
        .find(|l| printed(l).is_none() && l.contains(" exited unexpectedly ("))?;
    let status = line.split_once(" exited unexpectedly (")?.1;
    let status = status.strip_suffix(')').unwrap_or(status);
    if let Some(raw) = status
        .split_once("unix_wait_status(")
        .and_then(|(_, r)| r.split(')').next())
        .and_then(|n| n.parse::<i32>().ok())
    {
        let signal = raw & 0x7f;
        return Some(match signal {
            0 => format!("exit code {}", (raw >> 8) & 0xff),
            _ => format!("signal {signal}"),
        });
    }
    Some(cut(status, 80))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    #[test]
    fn the_database_keeps_severities_and_first_sentences_only() {
        let log = lines(
            "[stderr] 2026-11-16 10:00:00.123 UTC [812] LOG:  database system is ready to accept connections
[stderr] 2026-11-16 10:00:01.004 UTC [907] ERROR:  duplicate key value violates unique constraint \"trips_pkey\"
[stderr] 2026-11-16 10:00:01.004 UTC [907] DETAIL:  Key (title)=(Lisbon with my sister) already exists.
[stderr] 2026-11-16 10:00:01.004 UTC [907] STATEMENT:  insert into trips (title)
[stderr] \tvalues ('Lisbon with my sister')
[stderr] 2026-11-16 10:00:02.000 UTC [907] FATAL:  terminating connection. The server was asked to stop.
postgres exited unexpectedly (Ok(ExitStatus(unix_wait_status(256))))",
        );
        let kept = service_log(Source::Database, &log);
        assert_eq!(
            kept,
            [
                "LOG: database system is ready to accept connections",
                "ERROR: duplicate key value violates unique constraint \"[text removed]\"",
                "FATAL: terminating connection.",
                "postgres exited unexpectedly (Ok(ExitStatus(unix_wait_status(256))))",
            ]
        );
        assert!(kept.iter().all(|l| !l.contains("Lisbon")));
    }

    #[test]
    fn the_model_server_keeps_only_its_allowed_lines() {
        let log = lines(
            "spawned llama-server (pid 42) on 127.0.0.1:41000
[stderr] main: server is listening on http://127.0.0.1:41000
[stderr] srv  log_server_r: request: POST /v1/chat/completions 127.0.0.1 200
[stderr] slot update_slots: id  0 | task 3 | prompt: Plan one day in Lisbon
[stderr] error: failed to open \"what I typed into the box\"
[stderr] GGML_ASSERT(n_tokens <= n_batch) failed",
        );
        let kept = service_log(Source::ModelServer, &log);
        assert_eq!(
            kept,
            [
                "spawned llama-server (pid 42) on 127.0.0.1:41000",
                "main: server is listening on http://127.0.0.1:41000",
                "error: failed to open \"[text removed]\"",
                "GGML_ASSERT(n_tokens <= n_batch) failed",
            ]
        );
    }

    #[test]
    fn the_last_exit_reads_as_a_code_or_a_signal() {
        let exited = |s: &str| last_exit(&[format!("cache exited unexpectedly ({s})")]);
        assert_eq!(
            exited("Ok(ExitStatus(unix_wait_status(256)))").as_deref(),
            Some("exit code 1")
        );
        assert_eq!(
            exited("Ok(ExitStatus(unix_wait_status(9)))").as_deref(),
            Some("signal 9")
        );
        assert_eq!(
            last_exit(&lines("[stderr] x exited unexpectedly (1)")),
            None
        );
    }

    #[test]
    fn long_lines_are_cut() {
        let kept = service_log(Source::Other, &[format!("[stdout] {}", "y".repeat(500))]);
        assert_eq!(kept[0].chars().count(), LINE_LIMIT + 1);
    }
}
