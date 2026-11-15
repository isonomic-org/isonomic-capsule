//! One Chat turn against a running engine, timed. Not part of the hermetic
//! suite: it needs an engine with a model installed.
//!
//!     cargo run -p agents --example one_turn -- "<prompt>"
//!
//! Finds the engine's socket the way the window does (`ISONOMIC_CAPSULE_DIR`
//! or the per-user default), grants a test app (`com.isonomic.turn-check`),
//! creates a Chat session, sends the prompt and prints the reply as it
//! streams, then the time to the first text and to the end of the turn.

use std::time::Instant;

use isonomic_client::{unix_connector, Client, Options, SessionUpdate};
use isonomic_proto::manifest::AgentsAccess;
use isonomic_proto::{locate, Capabilities, Manifest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Say hello in one sentence.".into());
    let root: std::path::PathBuf = match std::env::var_os(locate::ENV_CAPSULE_DIR) {
        Some(dir) => dir.into(),
        None => {
            let home: std::path::PathBuf = std::env::var_os("HOME").ok_or("no HOME")?.into();
            locate::default_root(&home)
        }
    };
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let socket = locate::run_dir(&root, uid).join("engine.sock");
    println!("engine: {}", socket.display());

    let shell = Client::start(
        &tokio::runtime::Handle::current(),
        unix_connector(socket),
        Options::new("one-turn"),
    );
    let manifest = Manifest {
        id: "com.isonomic.turn-check".into(),
        name: "Turn check".into(),
        version: "0.1.0".into(),
        icon: "sparkle".into(),
        capabilities: Capabilities {
            agents: Some(AgentsAccess {}),
            ..Default::default()
        },
        schemas: vec![],
    };
    let app = shell.scoped(shell.grant(manifest).await?);
    let session = app.agents().create("chat").await?;
    let mut stream = app.agents().subscribe(&session, 0).await?;

    let sent = Instant::now();
    app.agents().send_text(&session, &prompt).await?;
    let mut first = None;
    while let Some(update) = stream.next().await {
        match update {
            SessionUpdate::Delta(delta) => {
                first.get_or_insert_with(|| sent.elapsed());
                print!("{}", delta.text);
                use std::io::Write;
                std::io::stdout().flush()?;
            }
            SessionUpdate::Event(event) => {
                let value = serde_json::to_value(&*event)?;
                match value["type"].as_str() {
                    Some("assistant") => {
                        first.get_or_insert_with(|| sent.elapsed());
                    }
                    Some("turn.end") => {
                        println!();
                        println!("turn.end: {}", value["stop"]);
                        if let Some(error) = value.get("error") {
                            println!("error: {error}");
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    let total = sent.elapsed();
    match first {
        Some(first) => println!("first text after {:.2} s", first.as_secs_f64()),
        None => println!("no text arrived"),
    }
    println!("turn took {:.2} s", total.as_secs_f64());
    // Leave nothing behind in the user's Capsule.
    app.agents().purge(None).await?;
    Ok(())
}
