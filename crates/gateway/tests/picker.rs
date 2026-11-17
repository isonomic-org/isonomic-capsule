// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the model picker needs from the gateway: which model is the
//! recommended one, and the check a session's chosen model must pass.

use std::sync::Arc;

use capsule_core::{RamDenial, RamGate};
use gateway::{Gateway, Machine};
use isonomic_model_library::Manifest;
use isonomic_proto::manifest::ModelAccess;
use isonomic_proto::{Capabilities, ErrorCode};

struct Open;
impl RamGate for Open {
    fn admit(&self, _: u64) -> Result<(), RamDenial> {
        Ok(())
    }
}

fn catalogue() -> Manifest {
    let hash = "0".repeat(64);
    let model = |id: &str, file: &str, size: u64, caps: &str| {
        format!(
            r#"{{"id":"{id}","name":"{id}","file_name":"{file}","source_url":"https://example.invalid/{file}","sha256":"{hash}","size_bytes":{size},"context_length":4096,"capabilities":[{caps}],"license":{{"name":"Apache-2.0","commercial_use":true,"url":"https://example.invalid"}}}}"#
        )
    };
    Manifest::from_json(&format!(
        r#"{{"default_chat":"chat-big","models":[{},{},{},{}]}}"#,
        model("chat-big", "big.gguf", 32, r#""chat""#),
        model("chat-small", "small.gguf", 16, r#""chat""#),
        model("embed", "embed.gguf", 8, r#""embeddings""#),
        model("absent", "absent.gguf", 12, r#""chat""#),
    ))
    .unwrap()
}

fn gateway(dir: &std::path::Path, slow: bool, total_memory_bytes: u64) -> Gateway {
    let models = dir.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for (file, size) in [("big.gguf", 32), ("small.gguf", 16), ("embed.gguf", 8)] {
        std::fs::write(models.join(file), vec![0u8; size]).unwrap();
    }
    Gateway::new(catalogue(), models, None, Arc::new(Open), slow).with_machine(Machine {
        profile: "standard".into(),
        total_memory_bytes,
    })
}

fn app(ids: &[&str]) -> isonomic_proto::Manifest {
    isonomic_proto::Manifest {
        id: "com.isonomic.chat".into(),
        name: "Chat".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            models: Some(ModelAccess {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        },
        schemas: vec![],
    }
}

#[test]
fn a_sessions_model_passes_the_streams_rule() {
    let dir = tempfile::tempdir().unwrap();
    let gateway = gateway(dir.path(), false, 16 << 30);
    let chat = app(&["chat-small", "absent", "embed"]);
    let code = |result: Result<String, isonomic_proto::WireError>| result.unwrap_err().code;
    assert_eq!(
        gateway
            .check_session_model(Some(&chat), "chat-small")
            .unwrap(),
        "chat-small"
    );
    assert_eq!(
        code(gateway.check_session_model(Some(&chat), "chat-big")),
        ErrorCode::CapabilityDenied
    );
    assert_eq!(
        code(gateway.check_session_model(Some(&chat), "absent")),
        ErrorCode::ModelUnavailable
    );
    assert_eq!(
        code(gateway.check_session_model(Some(&chat), "embed")),
        ErrorCode::Unsupported
    );
    assert_eq!(
        code(gateway.check_session_model(None, "nope")),
        ErrorCode::UnknownModel
    );
    assert_eq!(
        gateway.check_session_model(None, "chat-big").unwrap(),
        "chat-big"
    );
}

#[test]
fn the_recommended_model_follows_the_machine() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        gateway(dir.path(), false, 16 << 30)
            .recommended_id()
            .as_deref(),
        Some("chat-big")
    );
    // A slow machine without the 2B model in its manifest: the smallest chat model.
    assert_eq!(
        gateway(dir.path(), true, 16 << 30)
            .recommended_id()
            .as_deref(),
        Some("absent")
    );
    // Below the floor, nothing is recommended.
    assert_eq!(gateway(dir.path(), false, 8 << 30).recommended_id(), None);
}
