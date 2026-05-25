//! Minimal server-side JSON-RPC 2.0 + MCP wire types.
//!
//! `jarvis-mcp`'s protocol module declares the JSON-RPC structs as
//! `pub(crate)` (it's a client; it writes requests and reads responses).
//! Here we play the reverse role (read requests, write responses), so we
//! reimplement the minimal surface locally.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Inbound JSON-RPC 2.0 message. `id` is absent for notifications.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// Spec allows numbers or strings; we round-trip whatever we got.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Standard JSON-RPC error codes we care about.
pub mod err {
    pub const PARSE_ERROR: i64 = -32700;
    #[allow(dead_code)]
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    #[allow(dead_code)]
    pub const INTERNAL_ERROR: i64 = -32603;
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Outbound JSON-RPC 2.0 response. Exactly one of `result` / `error` is set.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// MCP tool descriptor — what `tools/list` returns.
#[derive(Debug, Clone, Serialize)]
pub struct McpToolDef {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Build a `tools/call` success result with a single text content block
/// containing the JSON-encoded value.
pub fn tool_text_result(value: &Value) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "isError": false,
    })
}

/// Build a `tools/call` error result. MCP convention: tool-level errors
/// come back as a normal result with `isError: true`, NOT as a JSON-RPC
/// error. Reserve JSON-RPC errors for protocol-level problems.
pub fn tool_error_result(message: impl Into<String>) -> Value {
    json!({
        "content": [
            { "type": "text", "text": message.into() }
        ],
        "isError": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_ok_serializes_without_error_field() {
        let r = JsonRpcResponse::ok(json!(1), json!({"x": 1}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"result\""));
        assert!(!s.contains("\"error\""));
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
    }

    #[test]
    fn response_error_serializes_without_result_field() {
        let r = JsonRpcResponse::error(json!(1), err::METHOD_NOT_FOUND, "nope");
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"error\""));
        assert!(!s.contains("\"result\""));
        assert!(s.contains("-32601"));
    }

    #[test]
    fn tool_text_result_wraps_json() {
        let r = tool_text_result(&json!({"hello": "world"}));
        assert_eq!(r["isError"], json!(false));
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("hello"));
    }

    #[test]
    fn tool_error_result_sets_is_error() {
        let r = tool_error_result("boom");
        assert_eq!(r["isError"], json!(true));
        assert_eq!(r["content"][0]["text"], "boom");
    }

    #[test]
    fn parses_request_with_numeric_id() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.method, "ping");
        assert_eq!(req.id, Some(json!(1)));
    }

    #[test]
    fn parses_request_with_string_id() {
        let raw = r#"{"jsonrpc":"2.0","id":"abc","method":"tools/list"}"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.id, Some(json!("abc")));
    }

    #[test]
    fn parses_notification_without_id() {
        let raw = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.id, None);
    }
}
