//! Build tasks. `cargo xtask fetch-sidecars` downloads the pinned services
//! into `sidecars/`, verifying each against `sidecars.json`.
//!
//!   (default)   fetch, verify and install every pinned service. On macOS
//!               that is both Apple slices, because the build is universal.
//!   --check     validate the manifest offline
//!   --record    download every declared target, write the hashes, and
//!               un-draft entries once all their targets are pinned
//!   --target=T  operate on one target triple only

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use packaging::install::{destination, install_one, Downloader, Outcome};
use packaging::{host_triple, Manifest, APPLE_SLICES};

struct Http;

impl Downloader for Http {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let response = ureq::get(url)
            .call()
            .map_err(|error| format!("GET {url} -> {error}"))?;
        let mut body = Vec::new();
        let read = response.into_reader().take(2 << 30).read_to_end(&mut body);
        read.map_err(|error| format!("GET {url} -> {error}"))?;
        Ok(body)
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the repo root")
        .to_path_buf()
}

fn triples(manifest: &Manifest, record: bool, only: Option<&str>) -> Vec<String> {
    if let Some(triple) = only {
        return vec![triple.to_string()];
    }
    if record {
        let mut all: Vec<String> = Vec::new();
        for sidecar in &manifest.sidecars {
            for triple in sidecar.targets.keys() {
                if !all.contains(triple) {
                    all.push(triple.clone());
                }
            }
        }
        return all;
    }
    if cfg!(target_os = "macos") {
        return APPLE_SLICES.iter().map(|t| t.to_string()).collect();
    }
    host_triple()
        .map(|t| vec![t.to_string()])
        .unwrap_or_default()
}

fn fetch_sidecars(args: &[String]) -> Result<(), String> {
    let check = args.iter().any(|a| a == "--check");
    let record = args.iter().any(|a| a == "--record");
    let only = args.iter().find_map(|a| a.strip_prefix("--target="));
    let root = repo_root();
    let path = root.join("sidecars.json");
    let mut manifest = Manifest::read(&path).map_err(|e| e.to_string())?;
    manifest.validate(!record).map_err(|e| e.to_string())?;
    if check {
        println!("sidecars.json: {} entries, valid", manifest.sidecars.len());
        return Ok(());
    }

    let mut changed = false;
    for triple in triples(&manifest, record, only) {
        println!("{triple}");
        for index in 0..manifest.sidecars.len() {
            let name = manifest.sidecars[index].name.clone();
            let version = manifest.sidecars[index].version.clone();
            match install_one(&root, &mut manifest, index, &triple, record, &Http)
                .map_err(|e| e.to_string())?
            {
                Outcome::Skipped(why) => println!("  {name}: skipped ({why})"),
                Outcome::AlreadyCurrent => println!("  {name}: already current"),
                Outcome::Installed => {
                    let dest = destination(&root, &manifest, &name, &triple);
                    println!("  {name} {version}: installed -> {}", dest.display());
                }
                Outcome::Recorded(hash) => {
                    changed = true;
                    println!("  {name}[{triple}] recorded sha256 {hash}");
                }
            }
        }
    }
    if record {
        for sidecar in &mut manifest.sidecars {
            if sidecar.is_draft() && sidecar.fully_pinned() {
                sidecar.draft = None;
                changed = true;
                println!("{}: every target pinned, no longer draft", sidecar.name);
            }
        }
        if changed {
            manifest.write(&path).map_err(|e| e.to_string())?;
            println!("sidecars.json updated. Check each hash against the vendor's published checksums before committing.");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("fetch-sidecars") => fetch_sidecars(&args[1..]),
        _ => Err(
            "usage: cargo xtask fetch-sidecars [--check] [--record] [--target=<triple>]"
                .to_string(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
