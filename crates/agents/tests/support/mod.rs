// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The engine as the engine binary assembles it, on a Capsule directory,
//! with the inference testkit's fake model server in place of llama.cpp.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agents::{deps::EngineLocale, EngineScopes, GatewayModel, Parts, SqliteSessionStore};
use broker::{Apps, Blobs, Events, Services, Sql, StackSystem, TokenKey};
use capsule_core::{CapsulePaths, FixedHardware, Profile, RamDenial, RamGate, SystemClock};
use gateway::Gateway;
use isonomic_client::{unix_connector, AppClient, Client, Options};
use isonomic_inference_testkit::write_script;
use isonomic_model_library::Manifest;
use isonomic_proto::manifest::AgentsAccess;
use isonomic_proto::Capabilities;
use registry::GrantStore;

pub struct Open;
impl RamGate for Open {
    fn admit(&self, _: u64) -> Result<(), RamDenial> {
        Ok(())
    }
}

pub fn catalogue() -> Manifest {
    let hash = "0".repeat(64);
    Manifest::from_json(&format!(
        r#"{{"default_chat":"chat-small","models":[{{"id":"chat-small","name":"Chat small","file_name":"small.gguf","source_url":"https://example.invalid/small.gguf","sha256":"{hash}","size_bytes":16,"context_length":4096,"capabilities":["chat"],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}]}}"#
    ))
    .unwrap()
}

/// Chat's manifest.
pub fn chat_manifest() -> isonomic_proto::Manifest {
    isonomic_proto::Manifest {
        id: "com.isonomic.chat".into(),
        name: "Chat".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            agents: Some(AgentsAccess {}),
            ..Default::default()
        },
        schemas: vec![],
    }
}

/// An engine on the Capsule at `root` with Chat granted; Chat's client.
pub async fn engine(root: &Path, port: u16) -> AppClient {
    let shell = shell(root, port).await;
    shell.scoped(shell.grant(chat_manifest()).await.unwrap())
}

/// An engine on the Capsule at `root`, its token key read from the
/// Capsule as the engine binary reads it; the shell's client.
pub async fn shell(root: &Path, port: u16) -> Client {
    shell_with(root, port, None).await
}

/// The same, with the engine's title rules on (`title_quiet`), as the
/// engine binary runs them.
pub async fn shell_with(root: &Path, port: u16, title_quiet: Option<Duration>) -> Client {
    let paths = CapsulePaths::new(root);
    let models = root.join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("small.gguf"), [0u8; 16]).unwrap();
    let program = write_script(root, "stand-in", "#!/bin/sh\nexec /bin/sleep 3600\n");
    let gateway = Arc::new(
        Gateway::new(catalogue(), models, Some(program), Arc::new(Open), false).with_configure(
            Box::new(move |config| {
                config.port = Some(port);
                config.idle_unload = None;
                config.probe_interval = Duration::from_millis(20);
            }),
        ),
    );
    let apps = Arc::new(Apps::new(
        GrantStore::open(&paths.registry_db()).unwrap(),
        TokenKey::load_or_create(&paths.token_key()).unwrap(),
    ));
    let host = agents::start(Parts {
        store: Arc::new(SqliteSessionStore::open(&paths.registry_db()).unwrap()),
        scopes: Arc::new(EngineScopes::new(Arc::clone(&apps), Arc::new(SystemClock))),
        model: Arc::new(match title_quiet {
            Some(quiet) => GatewayModel::new(Arc::clone(&gateway)).with_title_quiet(quiet),
            None => GatewayModel::new(Arc::clone(&gateway)),
        }),
        locale: EngineLocale {
            zone: "UTC".into(),
            offset_minutes: 0,
            language: "en".into(),
        },
        profile: Profile::Standard,
        engine_tools: false,
        gateway: Some(Arc::clone(&gateway)),
    })
    .await
    .unwrap();
    let identity = identity::IdentityService::new(identity::Person::new(
        root,
        &capsule_core::format::open(&paths, "test")
            .unwrap()
            .capsule_id,
        identity::memory_backend(),
        Arc::new(SystemClock),
    ))
    .with_apps(Arc::clone(&apps));
    let services = Services {
        apps,
        blobs: Blobs::open(&paths.blobs_dir(), &paths.registry_db()).unwrap(),
        events: Events::default(),
        sql: Sql::new(Arc::new(appdb::StandIn::default())),
        agents: Arc::new(host),
        models: gateway,
        identity: Arc::new(identity),
        connections: Arc::new(broker::NoConnections),
    };
    let system = StackSystem::new(
        Arc::new(supervisor::Stack::new(None)),
        None,
        Arc::new(FixedHardware::with_total(16 << 30)),
        Profile::Standard,
    );
    // A crashed engine leaves its socket file behind; binding takes over.
    let bound = broker::bind(&paths.engine_socket()).expect("the socket binds again");
    tokio::spawn(broker::serve(bound, system, Arc::new(services)));
    Client::start(
        &tokio::runtime::Handle::current(),
        unix_connector(paths.engine_socket()),
        Options::new("test"),
    )
}
