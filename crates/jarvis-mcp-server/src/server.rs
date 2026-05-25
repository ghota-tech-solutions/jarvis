//! Stdio main loop. Reads line-delimited JSON-RPC 2.0 messages from stdin,
//! dispatches to MCP handlers, writes responses to stdout. All logging goes
//! to stderr (stdout is reserved for the protocol).

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tonic::transport::{Channel, Endpoint};
use tracing::{debug, error, info, warn};

use jarvis_api::auth::{ClientAuth, discover_token};
use jarvis_api::jarvis_client::JarvisClient;

use crate::protocol::{JsonRpcRequest, JsonRpcResponse, err, tool_error_result, tool_text_result};
use crate::tools::{self, AuthedClient};

/// Default daemon endpoint, overridable via JARVIS_DAEMON_ADDR / JARVIS_DAEMON_URL.
const DEFAULT_DAEMON: &str = "http://127.0.0.1:7777";
/// Crate version, advertised in the `initialize` response.
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// MCP protocol revision we implement against.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Entry point used by `main.rs`.
pub async fn run() -> Result<()> {
    let endpoint = daemon_endpoint();
    info!(endpoint = %endpoint, "jarvis-mcp-server starting");

    // Build the gRPC client. If the daemon is unreachable or the token is
    // missing, log and continue — tools/call invocations will surface the
    // failure to the MCP client at call time.
    let mut client = match build_client(&endpoint).await {
        Ok(c) => {
            info!("connected to jarvis daemon");
            Some(c)
        }
        Err(e) => {
            warn!(
                error = %e,
                "could not connect to jarvis daemon at startup; tools/call will retry on each invocation"
            );
            None
        }
    };

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        let n = stdin.read_line(&mut line).await.context("stdin read")?;
        if n == 0 {
            info!("stdin closed, shutting down");
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        debug!(line = %trimmed, "received");

        // Lazily reconnect on demand if the startup connect failed.
        if client.is_none() {
            match build_client(&endpoint).await {
                Ok(c) => {
                    info!("late-connected to jarvis daemon");
                    client = Some(c);
                }
                Err(e) => {
                    debug!(error = %e, "still cannot reach daemon");
                }
            }
        }

        let resp = handle_line(trimmed, client.as_mut()).await;
        if let Some(r) = resp {
            let bytes = serde_json::to_vec(&r).context("encode response")?;
            stdout.write_all(&bytes).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

fn daemon_endpoint() -> String {
    if let Ok(v) = std::env::var("JARVIS_DAEMON_URL")
        && !v.trim().is_empty()
    {
        return v;
    }
    if let Ok(v) = std::env::var("JARVIS_DAEMON_ADDR")
        && !v.trim().is_empty()
    {
        // Allow bare host:port too — prepend http:// if missing.
        return if v.starts_with("http://") || v.starts_with("https://") {
            v
        } else {
            format!("http://{v}")
        };
    }
    DEFAULT_DAEMON.to_string()
}

async fn build_client(endpoint: &str) -> Result<AuthedClient> {
    let ep = Endpoint::from_shared(endpoint.to_string())
        .context("invalid daemon endpoint")?
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(3600));
    let channel: Channel = ep
        .connect()
        .await
        .with_context(|| format!("connect to {endpoint}"))?;
    let token = discover_token().unwrap_or_default();
    let auth = ClientAuth::new(&token).map_err(|e| anyhow::anyhow!("invalid token: {e}"))?;
    Ok(JarvisClient::with_interceptor(channel, auth))
}

/// Parse and dispatch a single JSON-RPC line. Returns the response to write
/// back, or `None` if the message was a pure notification (no id).
async fn handle_line(line: &str, client: Option<&mut AuthedClient>) -> Option<JsonRpcResponse> {
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "parse error");
            return Some(JsonRpcResponse::error(
                Value::Null,
                err::PARSE_ERROR,
                format!("parse error: {e}"),
            ));
        }
    };

    // Notifications have no id and must not get a response.
    let id = match req.id.clone() {
        Some(v) => v,
        None => {
            debug!(method = %req.method, "notification (no id), no response");
            return None;
        }
    };

    Some(dispatch(id, req, client).await)
}

async fn dispatch(
    id: Value,
    req: JsonRpcRequest,
    client: Option<&mut AuthedClient>,
) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => JsonRpcResponse::ok(id, initialize_result()),
        "ping" => JsonRpcResponse::ok(id, json!({})),
        "tools/list" => JsonRpcResponse::ok(id, tools_list_result()),
        "tools/call" => tools_call(id, req.params, client).await,
        other => {
            debug!(method = other, "method not found");
            JsonRpcResponse::error(
                id,
                err::METHOD_NOT_FOUND,
                format!("method not found: {other}"),
            )
        }
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "jarvis",
            "version": SERVER_VERSION,
        }
    })
}

fn tools_list_result() -> Value {
    json!({ "tools": tools::tool_defs() })
}

async fn tools_call(
    id: Value,
    params: Value,
    client: Option<&mut AuthedClient>,
) -> JsonRpcResponse {
    // Expected params shape: {"name":"<tool>", "arguments": {...}}
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => {
            return JsonRpcResponse::error(
                id,
                err::INVALID_PARAMS,
                "missing 'name' in tools/call params",
            );
        }
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let client = match client {
        Some(c) => c,
        None => {
            return JsonRpcResponse::ok(
                id,
                tool_error_result(
                    "jarvis daemon is unreachable; check that jarvis-daemon is running on \
                    127.0.0.1:7777 (or set JARVIS_DAEMON_URL) and that the bearer token is \
                    accessible (JARVIS_WEB_TOKEN or .jarvis/web.token).",
                ),
            );
        }
    };

    match tools::dispatch(client, &name, &args).await {
        Ok(value) => JsonRpcResponse::ok(id, tool_text_result(&value)),
        Err(e) => {
            warn!(tool = %name, error = ?e, "tool call failed");
            // MCP convention: tool-level errors come back as a normal result
            // with isError=true, not as a JSON-RPC error.
            JsonRpcResponse::ok(id, tool_error_result(format!("{e:#}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let resp = handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"bogus"}"#, None)
            .await
            .expect("expected a response");
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, err::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version() {
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            None,
        )
        .await
        .expect("expected a response");
        let result = resp.result.expect("result");
        assert_eq!(result["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], "jarvis");
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn ping_returns_empty_object() {
        let resp = handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#, None)
            .await
            .expect("expected a response");
        assert_eq!(resp.result, Some(json!({})));
    }

    #[tokio::test]
    async fn tools_list_returns_all_tools() {
        let resp = handle_line(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#, None)
            .await
            .expect("expected a response");
        let result = resp.result.expect("result");
        let tools = result["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 7);
    }

    #[tokio::test]
    async fn notification_returns_no_response() {
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            None,
        )
        .await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn parse_error_returns_jsonrpc_error_with_null_id() {
        let resp = handle_line("not json at all", None)
            .await
            .expect("expected a response");
        let err = resp.error.expect("error");
        assert_eq!(err.code, err::PARSE_ERROR);
        assert_eq!(resp.id, Value::Null);
    }

    #[tokio::test]
    async fn tools_call_without_daemon_returns_tool_error() {
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"jarvis_ping","arguments":{}}}"#,
            None,
        )
        .await
        .expect("expected a response");
        // Tool-level errors: still a result, with isError=true.
        let result = resp.result.expect("result");
        assert_eq!(result["isError"], json!(true));
    }

    #[tokio::test]
    async fn tools_call_missing_name_returns_invalid_params() {
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{}}"#,
            None,
        )
        .await
        .expect("expected a response");
        let err = resp.error.expect("error");
        assert_eq!(err.code, err::INVALID_PARAMS);
    }

    #[test]
    fn daemon_endpoint_prepends_http_to_bare_addr() {
        // SAFETY: tests run single-threaded by default; env mutation here is
        // bounded to this test.
        unsafe {
            std::env::remove_var("JARVIS_DAEMON_URL");
            std::env::set_var("JARVIS_DAEMON_ADDR", "10.0.0.1:7777");
        }
        let ep = daemon_endpoint();
        unsafe {
            std::env::remove_var("JARVIS_DAEMON_ADDR");
        }
        assert_eq!(ep, "http://10.0.0.1:7777");
    }
}
