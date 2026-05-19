//! End-to-end bench that proves the jarvis-mcp client + adapter chain works:
//! spawn mcp-mock, list its tools, call each, print results.
//!
//! Run with: `cargo run -p jarvis-mcp --bin mcp-bench`

use jarvis_mcp::{McpClient, McpServerSpec};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Locate the mcp-mock binary that cargo just built next to us.
    let exe = std::env::current_exe()?;
    let dir = exe.parent().unwrap();
    let mock = dir.join(if cfg!(windows) {
        "mcp-mock.exe"
    } else {
        "mcp-mock"
    });

    let client = McpClient::connect(McpServerSpec {
        name: "mock".to_string(),
        command: mock.to_string_lossy().to_string(),
        args: vec![],
        env: HashMap::new(),
        workdir: None,
    })
    .await?;

    println!("connected: {}", client.name());
    let tools = client.list_tools().await?;
    println!("tools/list: {}", tools.len());
    for t in &tools {
        println!("  {} — {}", t.name, t.description.as_deref().unwrap_or(""));
    }

    let echo = client
        .call_tool("echo", serde_json::json!({ "text": "salut Jarvis" }))
        .await?;
    println!(
        "echo → is_error={} content={:?}",
        echo.is_error,
        echo.flatten_text()
    );

    let add = client
        .call_tool("add", serde_json::json!({ "a": 40, "b": 2 }))
        .await?;
    println!(
        "add  → is_error={} content={:?}",
        add.is_error,
        add.flatten_text()
    );

    client.ping().await?;
    println!("ping ok");
    Ok(())
}
