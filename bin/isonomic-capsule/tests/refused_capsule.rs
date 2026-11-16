//! A Capsule the engine refuses gets no problem report and no engine log:
//! the engine stops before it sets either up.

use std::process::Command;

#[test]
fn a_refused_capsule_gets_no_report() {
    let dir = tempfile::tempdir().unwrap();
    let marker = r#"{"format": 99, "capsule_id": "3f9c0a7e1b2d4c5f8e6a7b9c0d1e2f3a", "created_by": "9.0.0", "last_opened_by": "9.0.0"}"#;
    std::fs::write(dir.path().join("capsule.json"), marker).unwrap();
    let engine = Command::new(env!("CARGO_BIN_EXE_isonomic-capsule"))
        .env_clear()
        .env("ISONOMIC_CAPSULE_DIR", dir.path())
        .env("HOME", dir.path())
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&engine.stderr);
    assert!(!engine.status.success(), "{log}");
    assert!(log.contains("newer version of Isonomic"), "{log}");
    assert!(!dir.path().join("reports").exists(), "{log}");
    assert!(
        !dir.path().join("logs/engine.log").exists(),
        "the engine writes its own log only into a Capsule it accepted"
    );
}
