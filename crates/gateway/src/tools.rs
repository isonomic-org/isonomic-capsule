// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tools on the wire, as the local model server takes them: the tools a
//! stream offers, and the calls an assistant message carries back.

use isonomic_inference_core::{ChatTool, ChatToolCall};
use isonomic_proto::models::{ToolCall, ToolSpec};
use isonomic_proto::{ErrorCode, WireError};

/// The tools a `models.stream` offers. A schema is JSON text on the wire;
/// one that is not a JSON object is refused, naming the tool.
pub(crate) fn offered(tools: Vec<ToolSpec>) -> Result<Vec<ChatTool>, WireError> {
    tools
        .into_iter()
        .map(|tool| {
            let parameters = if tool.schema.trim().is_empty() {
                serde_json::json!({ "type": "object" })
            } else {
                serde_json::from_str::<serde_json::Value>(&tool.schema)
                    .ok()
                    .filter(serde_json::Value::is_object)
                    .ok_or_else(|| {
                        let message =
                            format!("the schema of the tool {} is not a JSON object", tool.name);
                        WireError::new(ErrorCode::BadRequest, message)
                    })?
            };
            Ok(ChatTool {
                name: tool.name,
                description: tool.description,
                parameters,
            })
        })
        .collect()
}

/// One call an assistant message made, as the server reads it back.
pub(crate) fn call(call: ToolCall) -> ChatToolCall {
    ChatToolCall {
        id: call.id,
        name: call.name,
        arguments: call.arguments,
    }
}

/// A streamed call fragment, as the wire carries it.
pub(crate) fn delta(
    d: isonomic_inference_core::ToolCallDelta,
) -> isonomic_proto::models::ToolCallDelta {
    isonomic_proto::models::ToolCallDelta {
        index: d.index,
        id: d.id,
        name: d.name,
        arguments: d.arguments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(schema: &str) -> ToolSpec {
        ToolSpec {
            name: "clock".into(),
            description: "The time in a city".into(),
            schema: schema.into(),
        }
    }

    #[test]
    fn a_schema_becomes_the_tools_parameters() {
        let tools = offered(vec![spec(
            r#"{"type":"object","properties":{"city":{"type":"string"}}}"#,
        )])
        .unwrap();
        assert_eq!(tools[0].name, "clock");
        assert_eq!(tools[0].parameters["properties"]["city"]["type"], "string");
        let blank = offered(vec![spec("")]).unwrap();
        assert_eq!(blank[0].parameters, serde_json::json!({"type": "object"}));
    }

    #[test]
    fn a_schema_that_is_not_an_object_is_refused() {
        for bad in ["[1]", "not json"] {
            let e = offered(vec![spec(bad)]).unwrap_err();
            assert_eq!(e.code, ErrorCode::BadRequest);
            assert!(e.message.contains("clock"));
        }
    }
}
