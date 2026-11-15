//! A second engine for the same Capsule exits cleanly, so a login agent that
//! starts one while another is serving doesn't restart it in a loop.

use std::process::Command;

use capsule_core::CapsulePaths;

#[tokio::test]
async fn a_second_engine_exits_cleanly_while_the_first_serves() {
    let dir = tempfile::tempdir().unwrap();
    let paths = CapsulePaths::new(dir.path());
    // Stand in for the first engine: hold its socket and lock.
    let _first = broker::bind(&paths.engine_socket()).unwrap();

    let second = Command::new(env!("CARGO_BIN_EXE_isonomic-capsule"))
        .env_clear()
        .env("ISONOMIC_CAPSULE_DIR", dir.path())
        .env("HOME", dir.path())
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&second.stderr);
    assert!(second.status.success(), "{:?}: {log}", second.status);
    assert!(log.contains("another engine is already serving"), "{log}");
    assert!(log.contains("exiting"), "{log}");
}
