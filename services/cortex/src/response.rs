//! Shared LLM response parsing for the Cortex ReAct loop.
//!
//! Moved here from `handlers` so both `handlers` (telemetry / MCP-lite response assembly)
//! and `agent` (loop iteration logic) can call `parse_step_model_output` without a
//! circular dependency.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// Parsed output from one LLM turn inside the Cortex ReAct loop.
#[derive(Debug)]
pub struct StructuredStepOutput {
    /// "final" | "tool_call"
    pub response_type: String,
    /// Non-empty only when `response_type == "final"`.
    pub response_text: String,
    /// Non-null only when `response_type == "tool_call"`.
    /// Shape: `{"tool": "<name>", "arguments": {...}}`
    pub tool_call: Value,
}

/// Parse the raw string returned by the LLM into a `StructuredStepOutput`.
///
/// The model is instructed (via `build_structured_system_prompt`) to respond with
/// exactly one JSON object of type `"final"` or `"tool_call"`.
pub fn parse_step_model_output(raw: &str) -> Result<StructuredStepOutput> {
    let parsed: Value = serde_json::from_str(raw)
        .map_err(|err| anyhow!("cortex model output must be valid JSON: {err}"))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| anyhow!("cortex model output must be a JSON object"))?;
    let response_type = obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    match response_type.as_str() {
        "final" => {
            let content = obj
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if content.trim().is_empty() {
                return Err(anyhow!("final response requires non-empty content"));
            }
            Ok(StructuredStepOutput {
                response_type,
                response_text: content,
                tool_call: Value::Null,
            })
        }
        "tool_call" => {
            let tool = obj
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if tool.is_empty() {
                return Err(anyhow!("tool_call response requires tool"));
            }
            let arguments = obj
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !arguments.is_object() {
                return Err(anyhow!("tool_call arguments must be an object"));
            }
            Ok(StructuredStepOutput {
                response_type,
                response_text: String::new(),
                tool_call: json!({
                    "tool": tool,
                    "arguments": arguments,
                }),
            })
        }
        // "discover" is disabled — deterministic tool set only (Phase 2+).
        _ => Err(anyhow!("unsupported cortex response type: {}", response_type)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_final_output() {
        let parsed = parse_step_model_output(r#"{"type":"final","content":"hello"}"#)
            .expect("final output should parse");
        assert_eq!(parsed.response_type, "final");
        assert_eq!(parsed.response_text, "hello");
        assert!(parsed.tool_call.is_null());
    }

    #[test]
    fn parses_tool_call_output() {
        let parsed = parse_step_model_output(
            r#"{"type":"tool_call","tool":"browser.open","arguments":{"url":"https://weather.com"}}"#,
        )
        .expect("tool_call output should parse");
        assert_eq!(parsed.response_type, "tool_call");
        assert_eq!(parsed.tool_call["tool"].as_str(), Some("browser.open"));
        assert_eq!(
            parsed.tool_call["arguments"]["url"].as_str(),
            Some("https://weather.com")
        );
    }

    #[test]
    fn discover_type_is_rejected() {
        let result =
            parse_step_model_output(r#"{"type":"discover","query":"weather","kind":"all"}"#);
        assert!(result.is_err(), "discover type must be rejected while disabled");
    }

    #[test]
    fn empty_final_content_is_rejected() {
        let result = parse_step_model_output(r#"{"type":"final","content":""}"#);
        assert!(result.is_err());
    }

    #[test]
    fn tool_call_missing_tool_is_rejected() {
        let result = parse_step_model_output(r#"{"type":"tool_call","arguments":{}}"#);
        assert!(result.is_err());
    }
}
