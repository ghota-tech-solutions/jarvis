//! `jarvis-mcp-server` — exposes the running Jarvis daemon as an MCP
//! (Model Context Protocol) server over stdio.
//!
//! External MCP clients (Claude Code, Codex, Cline, ...) spawn this binary
//! and speak JSON-RPC 2.0 over its stdin/stdout. Each MCP `tools/call`
//! delegates to a gRPC RPC on the running Jarvis daemon.
//!
//! Logging goes to stderr. Stdout is reserved for protocol messages.

use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod protocol;
mod server;
mod tools;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    server::run().await
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("JARVIS_MCP_LOG")
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    // CRITICAL: never log to stdout — stdout is the MCP transport.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}
