//! jarvis-mcp — minimal Model Context Protocol client.
//!
//! Speaks JSON-RPC 2.0 line-delimited over a subprocess's stdin/stdout. Just
//! the methods we need to plug external MCP servers into our `ToolRegistry`:
//!   * `initialize`
//!   * `tools/list`
//!   * `tools/call`
//!   * `ping` (for health checks)
//!
//! Each `McpClient` owns the child process and a reader/writer task pair.
//! Requests are correlated by id via an in-memory map of pending oneshots.
//! Notifications (no id) are ignored for now.

mod adapter;
mod client;
mod protocol;

pub use adapter::McpToolAdapter;
pub use client::{McpClient, McpClientError, McpServerSpec};
pub use protocol::{McpTool, McpToolList};
