// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Problem reports: the engine's panic hook, and watches on the bundled
//! services and the model workers. Written to `reports/pending/` in the
//! Capsule in the window's format 1, for the window to show; nothing is
//! sent. Set up only once the Capsule has been opened: a Capsule that is
//! refused gets no report.

use std::sync::Arc;

use capsule_core::{CapsulePaths, Marker};
use reports::{Build, Reporter, System};
use supervisor::Stack;

/// The bundled services' versions, from the manifest they were fetched by.
const SIDECARS: &str = include_str!("../../../sidecars.json");
/// The SDK commit this engine was built against.
const SDK_REF: &str = include_str!("../../../SDK_REF");
/// The model server's sidecar name.
pub const MODEL_SERVER: &str = "llama-server";

/// A sidecar's pinned version; empty when the manifest does not list it.
pub fn sidecar_version(name: &str) -> String {
    let manifest: serde_json::Value = serde_json::from_str(SIDECARS).unwrap_or_default();
    manifest["sidecars"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["name"] == name)
        .and_then(|s| s["version"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// The sidecar a supervised service runs.
fn sidecar_of(service: &str) -> &str {
    match service {
        supervisor::cache::NAME => supervisor::cache::SIDECAR,
        other => other,
    }
}

fn short(commit: &str) -> String {
    commit.trim().chars().take(7).collect()
}

/// The reporter for this engine, with its panic hook installed. The build
/// number and the engine's commit are given by the release build
/// (`ISONOMIC_BUILD`, `ISONOMIC_ENGINE_COMMIT`); the engine does not know
/// Chat's commit, which the window's own reports carry.
pub fn install(paths: &CapsulePaths, marker: &Marker, total_memory_bytes: u64) -> Arc<Reporter> {
    let build = Build {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: option_env!("ISONOMIC_BUILD").unwrap_or("dev").to_string(),
        arch: std::env::consts::ARCH.to_string(),
        sdk: short(SDK_REF),
        chat: String::new(),
        engine: short(option_env!("ISONOMIC_ENGINE_COMMIT").unwrap_or_default()),
    };
    let system = System {
        os_version: reports::system::macos_version(),
        profile: None,
        memory_gb: Some(reports::system::memory_gb(total_memory_bytes)),
        capsule_format: Some(marker.format),
    };
    let home = std::env::var("HOME").ok().filter(|h| h.len() > 1);
    let reporter = Arc::new(Reporter::new(
        reports::store::Store::in_capsule(paths.root()),
        build,
        system,
        home,
    ));
    reports::panic::install(Arc::clone(&reporter));
    reporter
}

/// Note the hardware profile once it is chosen.
pub fn set_profile(reporter: &Reporter, profile: &str) {
    if let Ok(mut system) = reporter.system.lock() {
        system.profile = Some(profile.to_string());
    }
}

/// Watch every bundled service for its breaker.
pub fn watch_services(reporter: &Arc<Reporter>, stack: &Stack) {
    for (name, _, service) in stack.services() {
        let version = sidecar_version(sidecar_of(&name));
        reports::watch::service(Arc::clone(reporter), service, version);
    }
}

/// Told of each model worker the gateway makes, to watch it.
pub fn on_worker(reporter: &Arc<Reporter>) -> gateway::OnWorker {
    let reporter = Arc::clone(reporter);
    let version = sidecar_version(MODEL_SERVER);
    Box::new(move |model, worker| {
        if tokio::runtime::Handle::try_current().is_ok() {
            reports::watch::model_server(
                Arc::clone(&reporter),
                model.to_string(),
                Arc::clone(worker),
                MODEL_SERVER.to_string(),
                version.clone(),
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_versions_come_from_the_manifest() {
        assert_eq!(sidecar_version(sidecar_of("postgres")), "18.4.0");
        assert!(!sidecar_version(sidecar_of(supervisor::cache::NAME)).is_empty());
        assert!(!sidecar_version(MODEL_SERVER).is_empty());
        assert_eq!(sidecar_version("nothing"), "");
        assert_eq!(short(SDK_REF).len(), 7);
    }
}
