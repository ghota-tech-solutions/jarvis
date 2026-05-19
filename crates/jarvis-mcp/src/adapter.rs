//! Adapter that exposes an MCP tool as a `jarvis_tools::Tool`. One adapter per
//! tool — the agent sees them in the registry next to fs_read / fs_write / shell.

use crate::client::McpClient;
use crate::protocol::McpTool;
use async_trait::async_trait;
use jarvis_tools::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use serde_json::{Value, json};

pub struct McpToolAdapter {
    /// Fully-qualified name shown in the tool catalog, e.g. `filesystem.read_file`.
    name: String,
    description: String,
    args_schema: Value,
    client: McpClient,
    /// The bare tool name to pass to `tools/call` (without the `<server>.` prefix).
    bare_name: String,
}

impl McpToolAdapter {
    pub fn new(server: &str, tool: McpTool, client: McpClient) -> Self {
        let qualified = format!("{server}.{}", tool.name);
        let description = tool
            .description
            .clone()
            .unwrap_or_else(|| format!("MCP tool `{}` from server `{server}`", tool.name));
        Self {
            name: qualified,
            description,
            args_schema: tool.input_schema,
            bare_name: tool.name,
            client,
        }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            args_schema: self.args_schema.clone(),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let result = self
            .client
            .call_tool(&self.bare_name, args)
            .await
            .map_err(|e| ToolError::Other(format!("mcp: {e}")))?;
        let body = result.flatten_text();
        let summary = if result.is_error {
            format!("{} (error)", self.name)
        } else {
            format!("{} returned {} bytes", self.name, body.len())
        };
        let mut out = ToolOutput::ok(
            summary,
            json!({
                "tool": self.name,
                "is_error": result.is_error,
                "content": body,
            }),
        );
        out.is_error = result.is_error;
        Ok(out)
    }
}
