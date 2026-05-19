//! `spawn_subagent` tool — fork the agent loop into a focused child task.
//!
//! Implementation strategy : gRPC self-call. The tool opens a tonic client
//! against `JARVIS_DAEMON_URL` (default 127.0.0.1:7777), auto-discovers the
//! bearer token via `jarvis_api::auth::discover_token`, and calls
//! `SubmitTask` with `parent_task_id` set so the new task shows up under
//! the caller in the FleetDag and inherits workdir/sandbox/net.
//!
//! Two modes:
//! - **blocking = false (default)** : returns immediately with the child's
//!   `task_id`. The caller agent can keep going; the child runs in parallel.
//! - **blocking = true** : polls `GetTask` every 1s up to 5 min (default,
//!   configurable) and returns the child's final status + completion message.
//!
//! Role presets prefix the goal with a brief role description so the child
//! agent reads itself a system message about its scope. Codex-style roles :
//! - `explorer` : read-only research (fs_read / grep / glob / web_search /
//!   browser); never writes.
//! - `worker` : narrow edit, take ONE concrete action, then `done`.
//! - `reviewer` : verify-only. Reads. Reports ACK or NACK with reason.
//!   Never writes.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use jarvis_api::auth::{ClientAuth, discover_token};
use jarvis_api::{TaskHandle, TaskSpec, jarvis_client::JarvisClient};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::time::Duration;
use tonic::transport::{Channel, Endpoint};

pub struct SpawnSubagentTool;

#[derive(Debug, Deserialize)]
struct Args {
    goal: String,
    /// "explorer" | "worker" | "reviewer" (case-insensitive). Empty = pass
    /// the goal through unchanged.
    #[serde(default)]
    role: Option<String>,
    /// Override parent task id. Defaults to `ctx.current_task_id` set by
    /// the daemon's agent loop, which is what you want 99% of the time.
    #[serde(default)]
    parent_task_id: Option<String>,
    /// Override workdir; empty = inherit from parent.
    #[serde(default)]
    workdir: Option<String>,
    /// Cap on the child's agent loop iterations.
    #[serde(default)]
    max_steps: Option<u32>,
    /// If true, the tool blocks until the child reaches a terminal status.
    /// Default false → returns child's task_id immediately for fire-and-forget.
    #[serde(default)]
    blocking: Option<bool>,
    /// Polling timeout when `blocking = true`. Seconds. Default 300.
    #[serde(default)]
    timeout_s: Option<u64>,
}

const ROLE_EXPLORER: &str = "You are an EXPLORER sub-agent. \
    Your job is to investigate the codebase / docs / web and report back. \
    READ ONLY: fs_read, grep, glob, web_search, browser if available. \
    NEVER call fs_write, apply_patch, shell, or any side-effectful tool. \
    Return a concise summary of findings in your `done` message.";

const ROLE_WORKER: &str = "You are a WORKER sub-agent. \
    Take ONE concrete action to advance the stated goal, then `done`. \
    Prefer apply_patch with an @@ anchor over fs_write. \
    Do not branch off into adjacent work. \
    If the goal is impossible or larger than ONE action, emit `fail`.";

const ROLE_REVIEWER: &str = "You are a REVIEWER sub-agent. \
    Verify whether the stated change accomplishes the stated goal. \
    READ ONLY: fs_read, grep, glob, web_search. \
    Reply `done` with either `ACK: <one-line reason>` or `NACK: <reason>`. \
    Never modify files.";

fn role_prefix(role: Option<&str>) -> Option<&'static str> {
    let r = role?.to_ascii_lowercase();
    Some(match r.as_str() {
        "explorer" => ROLE_EXPLORER,
        "worker" => ROLE_WORKER,
        "reviewer" => ROLE_REVIEWER,
        _ => return None,
    })
}

#[async_trait]
impl Tool for SpawnSubagentTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "spawn_subagent".to_string(),
            description: "Fork the agent into a focused child task. \
                Pick a role (explorer|worker|reviewer) — the goal gets prefixed \
                with that role's system instructions. The child task appears \
                under this one in the FleetDag (parent inherited automatically). \
                Set blocking=true to wait for the child's verdict; otherwise \
                returns the child's task_id immediately for fan-out work."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "goal":           { "type": "string",  "description": "what the sub-agent should do" },
                    "role":           { "type": "string",  "enum": ["explorer", "worker", "reviewer"] },
                    "parent_task_id": { "type": "string",  "description": "override parent; defaults to the caller's task id" },
                    "workdir":        { "type": "string",  "description": "override workdir; empty = inherit" },
                    "max_steps":      { "type": "integer", "description": "cap on agent loop iterations" },
                    "blocking":       { "type": "boolean", "description": "wait for the child verdict (default false)" },
                    "timeout_s":      { "type": "integer", "description": "polling timeout when blocking (default 300s)" }
                },
                "required": ["goal"]
            }),
            // The tool itself just calls SubmitTask. The CHILD may have side
            // effects, but that's gated by its own sandbox_mode — not by this
            // tool's schema flag.
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: Args =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let goal_raw = a.goal.trim();
        if goal_raw.is_empty() {
            return Err(ToolError::InvalidArgs("goal is empty".into()));
        }

        // Build the goal with role prefix if a known role was requested.
        let goal = match role_prefix(a.role.as_deref()) {
            Some(prefix) => format!("{prefix}\n\nGoal: {goal_raw}"),
            None => goal_raw.to_string(),
        };

        let parent = a
            .parent_task_id
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ctx.current_task_id.clone());

        let mut client = connect_self_grpc().await.map_err(ToolError::Other)?;

        let spec = TaskSpec {
            goal,
            workdir: a.workdir.unwrap_or_default(),
            max_steps: a.max_steps.unwrap_or(20),
            sandbox: String::new(),
            net_policy: String::new(),
            use_worktree: false,
            base_ref: String::new(),
            routing_policy: String::new(),
            require_caps: Vec::new(),
            parent_task_id: parent,
            resume_from: String::new(),
        };

        let handle = client
            .submit_task(spec)
            .await
            .map_err(|s| ToolError::Other(format!("submit_task: {s}")))?
            .into_inner();

        if !a.blocking.unwrap_or(false) {
            return Ok(ToolOutput::ok(
                format!("spawned sub-agent {}", short(&handle.id)),
                json!({
                    "task_id": handle.id,
                    "blocking": false,
                }),
            ));
        }

        // Blocking mode: poll get_task every 1s until terminal status.
        let timeout = Duration::from_secs(a.timeout_s.unwrap_or(300));
        let start = std::time::Instant::now();
        let mut last_status = String::new();
        loop {
            if start.elapsed() > timeout {
                return Ok(ToolOutput::err(
                    format!(
                        "sub-agent {} timed out (still {})",
                        short(&handle.id),
                        last_status
                    ),
                    json!({
                        "task_id": handle.id,
                        "blocking": true,
                        "last_status": last_status,
                    }),
                ));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            let t = match client
                .get_task(TaskHandle {
                    id: handle.id.clone(),
                })
                .await
            {
                Ok(r) => r.into_inner(),
                Err(s) => {
                    return Ok(ToolOutput::err(
                        format!("sub-agent {}: get_task error {s}", short(&handle.id)),
                        json!({"task_id": handle.id, "error": s.to_string()}),
                    ));
                }
            };
            last_status = t.status.clone();
            let terminal = matches!(t.status.as_str(), "completed" | "failed" | "cancelled");
            if terminal {
                let ok = t.status == "completed";
                let summary = format!(
                    "sub-agent {} {} ({})",
                    short(&handle.id),
                    t.status,
                    t.error
                        .clone()
                        .unwrap_or_default()
                        .chars()
                        .take(60)
                        .collect::<String>(),
                );
                let data = json!({
                    "task_id": handle.id,
                    "status": t.status,
                    "error": t.error,
                    "completed_at": t.completed_at,
                });
                return Ok(if ok {
                    ToolOutput::ok(summary, data)
                } else {
                    ToolOutput::err(summary, data)
                });
            }
        }
    }
}

fn short(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_string()
}

async fn connect_self_grpc() -> Result<
    JarvisClient<tonic::service::interceptor::InterceptedService<Channel, ClientAuth>>,
    String,
> {
    let endpoint =
        std::env::var("JARVIS_DAEMON_URL").unwrap_or_else(|_| "http://127.0.0.1:7777".to_string());
    let token = discover_token().unwrap_or_default();
    let auth =
        ClientAuth::new(&token).map_err(|e| format!("invalid token (spawn_subagent): {e}"))?;
    let ep = Endpoint::from_shared(endpoint.clone())
        .map_err(|e| format!("endpoint {endpoint}: {e}"))?
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(3600));
    let channel = ep
        .connect()
        .await
        .map_err(|e| format!("connect {endpoint}: {e}"))?;
    Ok(JarvisClient::with_interceptor(channel, auth))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_prefix_canonical() {
        assert!(
            role_prefix(Some("explorer"))
                .unwrap()
                .starts_with("You are an EXPLORER")
        );
        assert!(
            role_prefix(Some("WORKER"))
                .unwrap()
                .starts_with("You are a WORKER")
        );
        assert!(
            role_prefix(Some("Reviewer"))
                .unwrap()
                .starts_with("You are a REVIEWER")
        );
        assert!(role_prefix(Some("unknown")).is_none());
        assert!(role_prefix(None).is_none());
    }

    #[test]
    fn short_id_strips_uuid_tail() {
        assert_eq!(short("ab1234cd-1234-5678-9abc-def012345678"), "ab1234cd");
    }
}
