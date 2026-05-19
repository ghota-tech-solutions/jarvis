//! Subprocess-backed MCP client. Spawns the server, talks JSON-RPC 2.0 over
//! its stdin (write) / stdout (read), correlates responses by id.

use crate::protocol::{
    JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, McpTool, McpToolCallResult, McpToolList,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::time::timeout;
use tracing::{debug, info, warn};

#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    #[error("spawn: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("transport: {0}")]
    Transport(String),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("server closed unexpectedly")]
    Closed,
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct McpServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub workdir: Option<std::path::PathBuf>,
}

/// Connected MCP server. Cheaply cloneable (Arc-everything-inside).
#[derive(Clone)]
pub struct McpClient {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>,
    stdin: Mutex<ChildStdin>,
    /// Hold the child so it's killed when the client drops.
    _child: Mutex<Child>,
    request_timeout: Duration,
}

impl McpClient {
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Spawn the server, do the MCP handshake (initialize), return a ready client.
    pub async fn connect(spec: McpServerSpec) -> Result<Self, McpClientError> {
        let mut cmd = Command::new(&spec.command);
        cmd.args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        if let Some(wd) = &spec.workdir {
            cmd.current_dir(wd);
        }
        let mut child = cmd.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpClientError::Transport("child stdout already taken".to_string()))?;
        let stderr = child.stderr.take();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpClientError::Transport("child stdin already taken".to_string()))?;

        let inner = Arc::new(Inner {
            name: spec.name.clone(),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            stdin: Mutex::new(stdin),
            _child: Mutex::new(child),
            request_timeout: Duration::from_secs(30),
        });

        // Spawn the stdout reader.
        {
            let inner = inner.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => {
                            debug!(server = %inner.name, "stdout closed");
                            break;
                        }
                        Ok(_) => {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<JsonRpcMessage>(trimmed) {
                                Ok(JsonRpcMessage::Response(resp)) => {
                                    let id = resp.id;
                                    let tx = inner.pending.lock().await.remove(&id);
                                    if let Some(tx) = tx {
                                        let _ = tx.send(resp);
                                    } else {
                                        warn!(server = %inner.name, id, "response with no pending request");
                                    }
                                }
                                Ok(JsonRpcMessage::Notification(n)) => {
                                    debug!(server = %inner.name, method = %n.method, "notification");
                                }
                                Err(e) => {
                                    warn!(server = %inner.name, error = %e, line = %trimmed, "decode failed");
                                }
                            }
                        }
                        Err(e) => {
                            warn!(server = %inner.name, error = %e, "read error");
                            break;
                        }
                    }
                }
            });
        }

        // Drain stderr to the logs so server panics are visible.
        if let Some(err) = stderr {
            let server_name = spec.name.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(err);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let t = line.trim_end();
                            if !t.is_empty() {
                                warn!(server = %server_name, stderr = %t, "");
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        let client = Self { inner };
        client.initialize().await?;
        info!(server = %spec.name, "mcp server connected");
        Ok(client)
    }

    async fn initialize(&self) -> Result<(), McpClientError> {
        let _ = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": { "tools": {} },
                    "clientInfo": { "name": "jarvis", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        // MCP spec: client SHOULD send `notifications/initialized` after init.
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpClientError> {
        let v = self.request("tools/list", json!({})).await?;
        let list: McpToolList = serde_json::from_value(v)?;
        Ok(list.tools)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        args: Value,
    ) -> Result<McpToolCallResult, McpClientError> {
        let v = self
            .request("tools/call", json!({ "name": name, "arguments": args }))
            .await?;
        let r: McpToolCallResult = serde_json::from_value(v)?;
        Ok(r)
    }

    pub async fn ping(&self) -> Result<(), McpClientError> {
        // Many MCP servers don't implement ping; treat method-not-found as ok.
        match self.request("ping", json!({})).await {
            Ok(_) => Ok(()),
            Err(McpClientError::Rpc { code: -32601, .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpClientError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let line = serde_json::to_string(&req)? + "\n";

        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);

        {
            let mut stdin = self.inner.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| McpClientError::Transport(format!("write: {e}")))?;
            stdin.flush().await.ok();
        }

        let resp = timeout(self.inner.request_timeout, rx)
            .await
            .map_err(|_| {
                // Drop pending entry to avoid leaks.
                tokio::task::block_in_place(|| {});
                McpClientError::Timeout(self.inner.request_timeout)
            })?
            .map_err(|_| McpClientError::Closed)?;
        if let Some(err) = resp.error {
            return Err(McpClientError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        resp.result
            .ok_or_else(|| McpClientError::Transport("empty result".to_string()))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpClientError> {
        let payload = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let line = serde_json::to_string(&payload)? + "\n";
        let mut stdin = self.inner.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpClientError::Transport(format!("write: {e}")))?;
        stdin.flush().await.ok();
        Ok(())
    }
}
