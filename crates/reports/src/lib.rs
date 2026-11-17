// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Problem reports from the engine, in format 1.
//!
//! A report is one small JSON document written to the Capsule's
//! `reports/pending/`, where the window keeps, shows and prunes its own. The
//! engine writes three kinds: a panic in the engine, `service_failed` when a
//! bundled service's breaker trips, and `model_server_failed` when a model
//! worker crashes or its breaker trips. Nothing here sends.
//!
//! The format, the scrubber, backtraces and the generic log rules are the
//! SDK's shared crate (`isonomic-reports`), the window's too. This crate
//! keeps what is the engine's:
//!
//! - [`filter`]: which rules each service's log follows, and the model
//!   server's allowed prefixes.
//! - [`store`]: writing a report beside the window's; [`Reporter`]: what
//!   every report shares.
//! - [`log`]: the engine's own log, whose last lines a report carries.
//! - [`panic`]: the panic hook. [`watch`]: services and model workers.

pub mod filter;
pub mod log;
pub mod panic;
pub mod reporter;
pub mod store;
pub mod system;
pub mod watch;

pub use isonomic_reports::{
    backtrace, scrub, Build, Component, Kind, Report, System, FORMAT, LOG_LINES,
};
pub use reporter::Reporter;
