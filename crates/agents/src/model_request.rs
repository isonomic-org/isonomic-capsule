// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The harness's model request as the gateway takes it: the system
//! message, the conversation with its tool calls and results, and the
//! tools offered.

use harness_core::ToolSpec as HarnessTool;
use harness_log::{Message as LogMessage, ModelRequest, Part, Role as LogRole};
use isonomic_proto::models::{Message, Role, StreamRequest, ToolCall, ToolSpec};

/// A message's words. Request context (the date, the language) is left
/// out: it goes to the system message, see [`request`]. Tool calls and
/// results are carried apart, see [`messages`].
pub(crate) fn text(parts: &[Part]) -> String {
    let mut out = String::new();
    for part in parts {
        let json;
        let piece = match part {
            Part::Text { text } | Part::Summary { text } => text,
            Part::Attachment { name, text, .. } => {
                out.push_str(&format!("[{name}]\n"));
                text
            }
            Part::Mention { label, .. } => label,
            // Structured data, such as a tool's result, as JSON text.
            Part::Json { value } => {
                json = serde_json::to_string(value).unwrap_or_default();
                &json
            }
            _ => continue,
        };
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(piece);
    }
    out
}

/// A tool the harness offers, as the wire describes it: its input schema
/// as JSON text.
fn tool(spec: &HarnessTool) -> ToolSpec {
    ToolSpec {
        name: spec.name.clone(),
        description: spec.description.clone(),
        schema: serde_json::to_string(&spec.input_schema).unwrap_or_default(),
    }
}

/// One harness message as the wire's messages. An assistant message keeps
/// its calls, by the harness's call id; a tool message becomes one message
/// per result, each answering its call.
fn messages(m: &LogMessage, out: &mut Vec<Message>) {
    match m.role {
        LogRole::Tool => {
            for part in &m.content {
                if let Part::ToolResult { call, content, .. } = part {
                    let mut result = text(content);
                    if result.is_empty() {
                        result = "(no result)".to_string();
                    }
                    out.push(Message {
                        role: Role::Tool,
                        content: result,
                        tool_calls: Vec::new(),
                        tool_call_id: Some(call.as_str().to_string()),
                    });
                }
            }
            // Words beside the results, if any, still reach the model.
            let words = text(&m.content);
            if !words.is_empty() {
                out.push(Message::user(words));
            }
        }
        role => {
            let tool_calls = m
                .content
                .iter()
                .filter_map(|p| match p {
                    Part::ToolCall { call, tool, input } => Some(ToolCall {
                        id: call.as_str().to_string(),
                        name: tool.clone(),
                        arguments: serde_json::to_string(input).unwrap_or_default(),
                    }),
                    _ => None,
                })
                .collect();
            out.push(Message {
                role: match role {
                    LogRole::Assistant => Role::Assistant,
                    _ => Role::User,
                },
                content: text(&m.content),
                tool_calls,
                tool_call_id: None,
            });
        }
    }
}

/// The harness's request as the gateway takes it.
///
/// The request context the harness puts in a user turn ("Current date: …")
/// moves to the end of the system message, latest only. Small local models
/// read it there as background; glued to the person's words, it made them
/// ask which day was meant instead of answering.
pub(crate) fn request(model: Option<String>, req: &ModelRequest) -> StreamRequest {
    let mut out = Vec::with_capacity(req.messages.len() + 1);
    let context = req.messages.iter().rev().find_map(|m| {
        m.content.iter().rev().find_map(|p| match p {
            Part::Context { text } => Some(text.clone()),
            _ => None,
        })
    });
    let system = match (req.system.is_empty(), context) {
        (true, None) => None,
        (false, None) => Some(req.system.clone()),
        (true, Some(context)) => Some(context),
        (false, Some(context)) => Some(format!("{}\n\n{context}", req.system)),
    };
    if let Some(system) = system {
        out.push(Message::system(system));
    }
    for m in &req.messages {
        messages(m, &mut out);
    }
    let reasoning = serde_json::to_value(&req.params.reasoning).ok();
    let off = reasoning.as_ref().and_then(|v| v.as_str()) == Some("off");
    StreamRequest {
        model,
        messages: out,
        tools: req.tools.iter().map(tool).collect(),
        temperature: None,
        max_tokens: Some(req.params.max_output_tokens).filter(|n| *n > 0),
        // Off when the preset says so; otherwise the engine decides.
        reasoning: off.then_some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_continue_request_ends_with_the_partial_reply() {
        let req: ModelRequest = serde_json::from_value(serde_json::json!({
            "model": "local:default",
            "params": {"max_output_tokens": 256, "reasoning": "off"},
            "system": "Be brief.",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Count to five"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "One, two,"}]}
            ],
            "prefill": true
        }))
        .unwrap();
        let out = request(Some("chat-small".into()), &req);
        let roles: Vec<_> = out.messages.iter().map(|m| m.role).collect();
        assert_eq!(out.messages[0].content, "Be brief.");
        assert_eq!(roles, [Role::System, Role::User, Role::Assistant]);
        assert_eq!(out.messages[2].content, "One, two,");
        assert_eq!(out.max_tokens, Some(256));
        assert_eq!(out.reasoning, Some(false));
        assert!(out.tools.is_empty());
    }

    #[test]
    fn the_request_context_moves_from_the_user_turn_to_the_system_message() {
        let req: ModelRequest = serde_json::from_value(serde_json::json!({
            "model": "local:default",
            "params": {"max_output_tokens": 64, "reasoning": "off"},
            "system": "Be brief.",
            "messages": [{"role": "user", "content": [
                {"type": "context", "text": "Current date: Sunday, 15 November 2026. Language: en."},
                {"type": "text", "text": "Plan one day in Lisbon."}
            ]}]
        }))
        .unwrap();
        let out = request(None, &req);
        assert_eq!(
            out.messages[0].content,
            "Be brief.\n\nCurrent date: Sunday, 15 November 2026. Language: en."
        );
        assert_eq!(out.messages[1].content, "Plan one day in Lisbon.");
    }

    #[test]
    fn tools_calls_and_results_reach_the_model_by_call_id() {
        let req: ModelRequest = serde_json::from_value(serde_json::json!({
            "model": "local:default",
            "params": {"max_output_tokens": 64, "reasoning": "off"},
            "system": "",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "What is in a.txt?"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "Reading it."},
                    {"type": "tool_call", "call": "c1", "tool": "read_file", "input": {"path": "a.txt"}},
                    {"type": "tool_call", "call": "c2", "tool": "list_dir", "input": {"dir": "."}}
                ]},
                {"role": "tool", "content": [
                    {"type": "tool_result", "call": "c1", "outcome": "ok",
                     "content": [{"type": "text", "text": "hello"}]},
                    {"type": "tool_result", "call": "c2", "outcome": "error",
                     "content": [{"type": "json", "value": {"error": "denied"}}]}
                ]}
            ],
            "tools": [{"name": "read_file", "description": "Read a file.",
                       "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}},
                       "kind": "engine", "concurrency": "parallel"}]
        }))
        .unwrap();
        let out = request(None, &req);
        assert_eq!(out.tools.len(), 1);
        assert_eq!(out.tools[0].name, "read_file");
        let schema: serde_json::Value = serde_json::from_str(&out.tools[0].schema).unwrap();
        assert_eq!(schema["properties"]["path"]["type"], "string");
        let assistant = &out.messages[1];
        assert_eq!(assistant.content, "Reading it.");
        assert_eq!(assistant.tool_calls.len(), 2);
        assert_eq!(assistant.tool_calls[0].id, "c1");
        assert_eq!(assistant.tool_calls[0].arguments, r#"{"path":"a.txt"}"#);
        assert_eq!(out.messages[2].role, Role::Tool);
        assert_eq!(out.messages[2].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(out.messages[2].content, "hello");
        assert_eq!(out.messages[3].tool_call_id.as_deref(), Some("c2"));
        assert_eq!(out.messages[3].content, r#"{"error":"denied"}"#);
        assert_eq!(out.messages.len(), 4);
    }
}
