//! Every manifest rule, each broken once.

use serde_json::json;

use super::*;

fn target(sha: &str) -> Value {
    json!({ "url": "https://example.test/a.tar.gz", "sha256": sha, "archive": "tar.gz", "pathInArchive": "a" })
}

const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn base() -> Value {
    json!({
        "$note": "kept on rewrite",
        "version": 1,
        "outDir": "sidecars",
        "sidecars": [{
            "name": "postgres",
            "version": "18.4.0",
            "license": "PostgreSQL",
            "install": "directory",
            "binaryInDir": "bin/postgres",
            "keepInBin": ["postgres", "initdb"],
            "targets": { "aarch64-apple-darwin": target(SHA), "x86_64-apple-darwin": target(SHA) }
        }]
    })
}

fn check(edit: impl FnOnce(&mut Value), require_hashes: bool) -> Result<(), String> {
    let mut value = base();
    edit(&mut value);
    let manifest: Manifest = serde_json::from_value(value).map_err(|e| e.to_string())?;
    manifest.validate(require_hashes).map_err(|e| e.to_string())
}

fn rejects(edit: impl FnOnce(&mut Value), needle: &str) {
    let error = check(edit, true).expect_err("should be rejected");
    assert!(error.contains(needle), "expected {needle:?} in {error:?}");
}

#[test]
fn a_complete_manifest_passes_and_survives_a_rewrite() {
    check(|_| {}, true).unwrap();
    let manifest: Manifest = serde_json::from_value(base()).unwrap();
    let rewritten: Value =
        serde_json::from_str(&serde_json::to_string(&manifest).unwrap()).unwrap();
    assert_eq!(rewritten, base());
}

#[test]
fn structural_rules() {
    rejects(|m| m["version"] = json!(2), "unsupported manifest version");
    rejects(|m| m["outDir"] = json!(""), "outDir");
    rejects(
        |m| m["sidecars"][0]["name"] = json!("Post_gres"),
        "kebab-case",
    );
    rejects(
        |m| {
            let copy = m["sidecars"][0].clone();
            m["sidecars"].as_array_mut().unwrap().push(copy);
        },
        "duplicate sidecar",
    );
    rejects(
        |m| m["sidecars"][0]["version"] = json!(""),
        "version is required",
    );
    rejects(
        |m| m["sidecars"][0]["license"] = json!(""),
        "license is required",
    );
    rejects(
        |m| m["sidecars"][0]["install"] = json!("folder"),
        "install must be",
    );
    rejects(
        |m| {
            m["sidecars"][0]
                .as_object_mut()
                .unwrap()
                .remove("binaryInDir");
        },
        "binaryInDir is required",
    );
    rejects(
        |m| {
            m["sidecars"][0].as_object_mut().unwrap().remove("install");
        },
        "binaryInDir only applies",
    );
    // A tree may be built from source: the build leaves the installed tree.
    check(
        |m| m["sidecars"][0]["build"] = json!({ "command": "make", "producesPath": "x" }),
        true,
    )
    .unwrap();
    // One target's own build, in place of the sidecar's.
    rejects(
        |m| {
            m["sidecars"][0]["targets"]["x86_64-apple-darwin"]["build"] =
                json!({ "command": "make" })
        },
        "producesPath is required",
    );
    rejects(
        |m| m["minimumMacos"] = json!("thirteen"),
        "minimumMacos must be a macOS version",
    );
    check(|m| m["minimumMacos"] = json!("13.3"), true).unwrap();
}

#[test]
fn target_rules() {
    rejects(
        |m| {
            m["sidecars"][0]["targets"]
                .as_object_mut()
                .unwrap()
                .remove("x86_64-apple-darwin");
        },
        "the macOS build is universal",
    );
    rejects(
        |m| m["sidecars"][0]["targets"]["universal-apple-darwin"] = target(SHA),
        "do not declare",
    );
    rejects(
        |m| m["sidecars"][0]["targets"]["x86_64-apple-darwin"]["url"] = json!("http://x"),
        "must be https://",
    );
    rejects(
        |m| m["sidecars"][0]["targets"]["x86_64-apple-darwin"]["archive"] = json!("rar"),
        "archive must be one of",
    );
    rejects(
        |m| m["sidecars"][0]["targets"]["x86_64-apple-darwin"]["archive"] = json!("none"),
        "bare download",
    );
    rejects(
        |m| {
            m["sidecars"][0]["targets"]["x86_64-apple-darwin"]
                .as_object_mut()
                .unwrap()
                .remove("pathInArchive");
        },
        "pathInArchive is required",
    );
    rejects(
        |m| m["sidecars"][0]["targets"]["x86_64-apple-darwin"]["sha256"] = json!("ABC"),
        "64 lowercase hex",
    );
}

#[test]
fn a_missing_hash_fails_closed_unless_draft_or_recording() {
    let unpin =
        |m: &mut Value| m["sidecars"][0]["targets"]["x86_64-apple-darwin"]["sha256"] = json!("");
    rejects(unpin, "sha256 is empty");
    check(unpin, false).unwrap(); // recording mode
    check(
        |m| {
            unpin(m);
            m["sidecars"][0]["draft"] = json!(true);
        },
        true,
    )
    .unwrap();
}

#[test]
fn pinning_fills_a_target_and_reports_completeness() {
    let mut value = base();
    value["sidecars"][0]["targets"]["x86_64-apple-darwin"]["sha256"] = json!("");
    let mut manifest: Manifest = serde_json::from_value(value).unwrap();
    let sidecar = &mut manifest.sidecars[0];
    assert!(!sidecar.fully_pinned());
    sidecar.pin("x86_64-apple-darwin", SHA);
    assert!(sidecar.fully_pinned());
    assert_eq!(sidecar.target("x86_64-apple-darwin").unwrap().sha256, SHA);
}
