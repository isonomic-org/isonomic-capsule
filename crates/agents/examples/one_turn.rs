// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Timed Chat turns against a running engine. Not part of the hermetic
//! suite: it needs an engine with a model installed.
//!
//!     one_turn [--turns N] ["<prompt>"]
//!
//! Finds the engine's socket the way the window does (`ISONOMIC_CAPSULE_DIR`
//! or the per-user default). First it times loading the default chat model
//! on its own: one 1-token stream (`model load`), so the turns that follow
//! are warm. Then it runs N turns (3 by default) in one Chat session of a
//! test app (`com.isonomic.turn-check`) and prints, per turn, the time to
//! the first text, the whole turn, the tokens and tokens per second. Tokens
//! are the server's streamed chunks, about one token each. Everything the
//! test app made is purged at the end.

use std::io::Write;
use std::time::{Duration, Instant};

use isonomic_client::{unix_connector, Client, Options, SessionUpdate};
use isonomic_proto::manifest::AgentsAccess;
use isonomic_proto::models::{Message, StreamRequest};
use isonomic_proto::{locate, Capabilities, Manifest};

fn args() -> (usize, String) {
    let mut turns = 3;
    let mut prompt = "Plan one day in Lisbon in three short lines.".to_string();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--turns" => turns = it.next().and_then(|n| n.parse().ok()).unwrap_or(turns),
            _ => prompt = arg,
        }
    }
    (turns.max(1), prompt)
}

fn secs(d: Duration) -> String {
    format!("{:.2} s", d.as_secs_f64())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (turns, prompt) = args();
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
    let runtime = tokio::runtime::Handle::current();
    let shell = Client::start(&runtime, unix_connector(socket), Options::new("one-turn"));

    // Cold: load the default chat model on its own, with one token.
    let started = Instant::now();
    let load = StreamRequest {
        messages: vec![Message::user("Hi")],
        max_tokens: Some(1),
        ..Default::default()
    };
    let mut warmup = shell.models().stream(load).await?;
    let model = warmup.model().to_string();
    while let Some(chunk) = warmup.next().await {
        chunk?;
    }
    println!("model: {model}");
    println!("model load (and one token): {}", secs(started.elapsed()));

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

    for turn in 1..=turns {
        println!("--- turn {turn}: {prompt}");
        let sent = Instant::now();
        app.agents().send_text(&session, &prompt).await?;
        let mut first = None;
        let mut tokens = None;
        while let Some(update) = stream.next().await {
            match update {
                SessionUpdate::Ended(reason) => {
                    return Err(format!("the session ended: {reason:?}").into())
                }
                SessionUpdate::Gap { .. } => println!("(missed some updates)"),
                SessionUpdate::Delta(delta) => {
                    first.get_or_insert_with(|| sent.elapsed());
                    print!("{}", delta.text);
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
                            tokens = value["usage"]["output_tokens"].as_u64();
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
        let total = sent.elapsed();
        let first_text = first.map_or("none".to_string(), secs);
        let tokens = tokens.unwrap_or(0);
        let generating = first.map_or(total, |f| total.saturating_sub(f));
        let rate = if tokens > 0 && !generating.is_zero() {
            format!("{:.1} tok/s", tokens as f64 / generating.as_secs_f64())
        } else {
            "n/a".to_string()
        };
        println!(
            "turn {turn}: first text {first_text}, total {}, {tokens} tokens, {rate}",
            secs(total)
        );
    }
    // Leave nothing behind in the Capsule.
    app.agents().purge(None).await?;
    Ok(())
}
