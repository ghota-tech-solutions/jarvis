//! JSON-RPC 2.0 wire types + the MCP-specific request/response payloads we use.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcRequest<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    pub params: Value,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum JsonRpcMessage {
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[allow(dead_code)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

/// MCP tool definition as returned by `tools/list`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema object describing the tool's args.
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpToolList {
    pub tools: Vec<McpTool>,
}

/// Response shape of `tools/call`. MCP returns either a list of content blocks
/// (`{"content": [{"type":"text", "text":"…"}], "isError": false}`) or a
/// shorter error variant. We surface both to the agent as a plain string.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolCallResult {
    #[serde(default)]
    pub content: Vec<McpContent>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContent {
    Text {
        text: String,
    },
    Image {
        #[serde(default)]
        data: String,
        #[serde(default)]
        mime_type: String,
    },
    Resource {
        #[serde(default)]
        resource: Value,
    },
    /// Anything we don't recognise — keep it as raw JSON so we don't drop info.
    #[serde(other)]
    Other,
}

impl McpToolCallResult {
    /// Flatten the content blocks into a single string for the agent.
    pub fn flatten_text(&self) -> String {
        let mut out = String::new();
        for c in &self.content {
            match c {
                McpContent::Text { text } => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
                McpContent::Image { mime_type, .. } => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&format!("[image: {mime_type}]"));
                }
                McpContent::Resource { .. } => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str("[resource]");
                }
                McpContent::Other => {}
            }
        }
        out
    }
}
