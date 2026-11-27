// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Streamed tool-call fragments joined into whole calls. Fragments with the
//! same index belong to one call; the first names it, the rest append to
//! its arguments. A fragment with no index of its own cannot occur in the
//! chunk type, so calls are keyed by index alone. Kept to this one type so
//! the inference library's own joiner can replace it.

use std::collections::BTreeMap;

use isonomic_inference_core::ToolCallDelta;

#[derive(Debug, Default)]
pub(crate) struct ToolCalls {
    calls: BTreeMap<u32, (String, String)>,
}

impl ToolCalls {
    pub(crate) fn push(&mut self, delta: ToolCallDelta) {
        let call = self.calls.entry(delta.index).or_default();
        if let Some(name) = delta.name.filter(|n| !n.is_empty()) {
            call.0 = name;
        }
        call.1.push_str(&delta.arguments);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Each call's tool and arguments, in index order. An empty argument
    /// text is an empty object.
    pub(crate) fn finish(self) -> Vec<(String, String)> {
        self.calls
            .into_values()
            .map(|(tool, args)| {
                let args = if args.trim().is_empty() {
                    "{}".to_string()
                } else {
                    args
                };
                (tool, args)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(index: u32, name: Option<&str>, args: &str) -> ToolCallDelta {
        ToolCallDelta {
            index,
            id: name.map(|_| format!("call_{index}")),
            name: name.map(str::to_string),
            arguments: args.to_string(),
        }
    }

    #[test]
    fn interleaved_fragments_join_by_index() {
        let mut calls = ToolCalls::default();
        for delta in [
            d(0, Some("read_file"), ""),
            d(0, None, "{\"pa"),
            d(1, Some("list_dir"), "{\"dir"),
            d(0, None, "th\":\"a.txt\"}"),
            d(1, None, "\":\".\"}"),
            d(2, Some("now"), ""),
        ] {
            calls.push(delta);
        }
        assert_eq!(
            calls.finish(),
            [
                ("read_file".to_string(), r#"{"path":"a.txt"}"#.to_string()),
                ("list_dir".to_string(), r#"{"dir":"."}"#.to_string()),
                ("now".to_string(), "{}".to_string()),
            ]
        );
    }
}
