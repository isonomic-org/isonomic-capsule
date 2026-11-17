// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a chat worker is launched: its slots, its context and, for the
//! Qwen3.5 family, the chat template the conversation is rendered with.

use std::path::{Path, PathBuf};

use isonomic_llama::LlamaConfig;
use isonomic_model_library::ModelEntry;

/// The chat workers' default sampling.
pub const CHAT_SAMPLING: &[&str] = &[
    "--temp", "0.7", "--top-p", "0.8", "--top-k", "20", "--min-p", "0",
];

/// Request slots per chat worker. A conversation keeps one slot's cache
/// between turns; the title step, which renders a different prompt, takes
/// the other one instead of overwriting it.
pub const CHAT_SLOTS: u32 = 2;

/// Tokens of context in each chat slot. The server divides `-c` between
/// its slots, so a worker is launched with `CHAT_SLOTS` times this.
pub const SLOT_CONTEXT: u32 = 8192;

/// The Qwen3.5 models' own chat template with one change: earlier replies
/// keep the empty thinking block the model saw when it wrote them. The
/// original drops it from every reply before the latest request, so the
/// server's cache of the conversation stops matching at the start of the
/// previous reply and that whole reply is read again before every turn.
const HISTORY_KEPT: &str = include_str!("../templates/history-kept.jinja");

/// The models that template is for, by catalogue id prefix. Checked
/// identical in Qwen3.5 0.8B and 2B, which carry the same template.
const HISTORY_KEPT_FAMILY: &str = "qwen3.5-";

/// Set a chat worker's launch up for `entry`. The template, when the model
/// takes one, is written into `templates/` under the model directory.
pub fn configure(config: &mut LlamaConfig, entry: &ModelEntry, models_dir: &Path) {
    config.parallel = CHAT_SLOTS;
    config.context_size = SLOT_CONTEXT * CHAT_SLOTS;
    // The server's sampling defaults, set to what the Qwen3.5 model cards
    // recommend without thinking; a request's own temperature still wins.
    config
        .extra_args
        .extend(CHAT_SAMPLING.iter().map(|a| a.to_string()));
    if !entry.meta.id.as_str().starts_with(HISTORY_KEPT_FAMILY) {
        return;
    }
    match write_template(models_dir) {
        Ok(path) => config.extra_args.extend([
            "--jinja".to_string(),
            "--chat-template-file".to_string(),
            path.to_string_lossy().into_owned(),
        ]),
        // The model's own template still works, only slower between turns.
        Err(e) => capsule_core::log::line(&format!("warning: chat template not written: {e}")),
    }
}

fn write_template(models_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = models_dir.join("templates");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("history-kept.jinja");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(HISTORY_KEPT) {
        let part = dir.join("history-kept.jinja.part");
        std::fs::write(&part, HISTORY_KEPT)?;
        std::fs::rename(&part, &path)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earlier_replies_keep_their_thinking_block() {
        // The one line that differs from the model's own template.
        assert!(HISTORY_KEPT.contains("{%- if true %}"));
        assert!(!HISTORY_KEPT.contains("loop.index0 > ns.last_query_index"));
        assert!(HISTORY_KEPT.contains("'<think>\\n\\n</think>\\n\\n'"));
    }

    #[test]
    fn the_template_is_written_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_template(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), HISTORY_KEPT);
        std::fs::write(&path, "changed").unwrap();
        assert_eq!(write_template(dir.path()).unwrap(), path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), HISTORY_KEPT);
    }

    #[test]
    fn the_qwen_family_gets_the_template_and_others_their_own() {
        let manifest = isonomic_model_library::Manifest::builtin();
        let dir = tempfile::tempdir().unwrap();
        let launch = |id: &str| {
            let entry = manifest
                .entries()
                .iter()
                .find(|e| e.meta.id.as_str() == id)
                .unwrap();
            let mut config = LlamaConfig::new("server".into(), "model.gguf".into());
            configure(&mut config, entry, dir.path());
            config
        };
        let qwen = launch("qwen3.5-2b-q4-k-m");
        assert_eq!((qwen.parallel, qwen.context_size), (2, 16384));
        let at = qwen
            .extra_args
            .iter()
            .position(|a| a == "--chat-template-file")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&qwen.extra_args[at + 1]).unwrap(),
            HISTORY_KEPT
        );
        let other = launch("smollm3-3b-q4-k-m");
        assert!(!other.extra_args.iter().any(|a| a == "--chat-template-file"));
        assert!(qwen.validate().is_ok() && other.validate().is_ok());
    }
}
