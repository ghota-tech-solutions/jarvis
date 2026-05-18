//! Minimal MCP server for end-to-end smoke testing. Speaks JSON-RPC 2.0 over
//! stdin/stdout, advertises two tools:
//!   * `echo`   — returns its `text` arg verbatim.
//!   * `add`    — returns `a + b` as text.
//!
//! That's enough to exercise initialize → tools/list → tools/call.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut lines = stdin.lock().lines();
    while let Some(Ok(line)) = lines.next() {
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        // Notifications have no id — process and don't respond.
        if id.is_none() {
            continue;
        }
        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mcp-mock", "version": "1.0" }
            }),
            "tools/list" => json!({
                "tools": [
                    {
                        "name": "echo",
                        "description": "Return the input text verbatim.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "text": { "type": "string" } },
                            "required": ["text"]
                        }
                    },
                    {
                        "name": "add",
                        "description": "Add two numbers.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "a": { "type": "number" },
                                "b": { "type": "number" }
                            },
                            "required": ["a", "b"]
                        }
                    }
                ]
            }),
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                match name {
                    "echo" => {
                        let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        json!({
                            "content": [{ "type": "text", "text": text }],
                            "isError": false
                        })
                    }
                    "add" => {
                        let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        json!({
                            "content": [{ "type": "text", "text": format!("{}", a + b) }],
                            "isError": false
                        })
                    }
                    _ => {
                        let resp = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": format!("unknown tool: {name}") }
                        });
                        let _ = writeln!(out, "{resp}");
                        let _ = out.flush();
                        continue;
                    }
                }
            }
            "ping" => json!({}),
            _ => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("method not found: {method}") }
                });
                let _ = writeln!(out, "{resp}");
                let _ = out.flush();
                continue;
            }
        };
        let resp = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let _ = writeln!(out, "{resp}");
        let _ = out.flush();
    }
}
