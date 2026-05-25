//! MCP tool definitions + handlers. Each handler wraps a single gRPC RPC
//! exposed by the Jarvis daemon.

use anyhow::{Context, Result};
use futures::StreamExt;
use jarvis_api::{
    AskRequest, ListTasksRequest, PingRequest, StatusRequest, TaskHandle, TaskSpec,
    auth::ClientAuth, jarvis_client::JarvisClient,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;

use crate::protocol::McpToolDef;

/// Type of the authenticated gRPC client we share across tool invocations.
pub type AuthedClient = JarvisClient<InterceptedService<Channel, ClientAuth>>;

/// Static list of MCP tools exposed by this server.
pub fn tool_defs() -> Vec<McpToolDef> {
    vec![
        McpToolDef {
            name: "jarvis_ping",
            description: "Health-check the Jarvis daemon. Returns the daemon version and uptime.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        },
        McpToolDef {
            name: "jarvis_ask",
            description: "One-shot LLM ask, streamed via the daemon. The streamed deltas are collected and returned as a single string. Bypasses the agent loop.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "The prompt text." },
                    "model":  { "type": "string", "description": "Optional provider/model name." },
                },
                "required": ["prompt"],
                "additionalProperties": false,
            }),
        },
        McpToolDef {
            name: "jarvis_submit_task",
            description: "Submit a new agent task to the Jarvis daemon. Returns the task id.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "goal":    { "type": "string", "description": "Goal description for the agent." },
                    "workdir": { "type": "string", "description": "Working directory. Defaults to the daemon's cwd if omitted." },
                    "routing": { "type": "string", "description": "Routing policy: auto | local_only | remote_only | model:<name>." },
                },
                "required": ["goal"],
                "additionalProperties": false,
            }),
        },
        McpToolDef {
            name: "jarvis_get_task",
            description: "Fetch the current state of a task by id.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "Task id (UUID)." }
                },
                "required": ["task_id"],
                "additionalProperties": false,
            }),
        },
        McpToolDef {
            name: "jarvis_list_tasks",
            description: "List tasks known to the daemon. By default only active tasks; pass all=true to include finished ones.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "all": { "type": "boolean", "description": "Include completed/failed/cancelled tasks. Default false." }
                },
                "additionalProperties": false,
            }),
        },
        McpToolDef {
            name: "jarvis_cancel_task",
            description: "Request cancellation of a running task by id.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "Task id (UUID)." }
                },
                "required": ["task_id"],
                "additionalProperties": false,
            }),
        },
        McpToolDef {
            name: "jarvis_status",
            description: "Daemon-wide status snapshot: version, uptime, registered models, count of running tasks.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        },
    ]
}

/// Dispatch a `tools/call` invocation. Returns the JSON value of the tool
/// result on success, or an `Err` whose message will be surfaced as a
/// tool-level error (`isError: true`).
pub async fn dispatch(client: &mut AuthedClient, name: &str, args: &Value) -> Result<Value> {
    match name {
        "jarvis_ping" => call_ping(client).await,
        "jarvis_ask" => call_ask(client, args).await,
        "jarvis_submit_task" => call_submit_task(client, args).await,
        "jarvis_get_task" => call_get_task(client, args).await,
        "jarvis_list_tasks" => call_list_tasks(client, args).await,
        "jarvis_cancel_task" => call_cancel_task(client, args).await,
        "jarvis_status" => call_status(client).await,
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

// ---------- handlers ----------

async fn call_ping(client: &mut AuthedClient) -> Result<Value> {
    let r = client
        .ping(PingRequest {})
        .await
        .context("daemon Ping rpc failed")?
        .into_inner();
    Ok(json!({
        "version": r.version,
        "uptime_seconds": r.uptime_seconds,
    }))
}

#[derive(Deserialize)]
struct AskArgs {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
}

async fn call_ask(client: &mut AuthedClient, args: &Value) -> Result<Value> {
    let a: AskArgs = serde_json::from_value(args.clone()).context("invalid args for jarvis_ask")?;
    let req = AskRequest {
        prompt: a.prompt,
        provider: a.model.unwrap_or_default(),
        temperature: None,
        max_tokens: None,
    };
    let mut stream = client
        .ask(req)
        .await
        .context("daemon Ask rpc failed")?
        .into_inner();
    let mut text = String::new();
    let mut usage: Option<jarvis_api::UsageStats> = None;
    let mut finish_reason: Option<String> = None;
    while let Some(item) = stream.next().await {
        let chunk = item.context("ask stream error")?;
        if !chunk.delta.is_empty() {
            text.push_str(&chunk.delta);
        }
        if chunk.usage.is_some() {
            usage = chunk.usage;
        }
        if let Some(reason) = chunk.finish_reason {
            finish_reason = Some(reason);
        }
    }
    let usage_json = usage.map(|u| {
        json!({
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens,
        })
    });
    Ok(json!({
        "text": text,
        "usage": usage_json,
        "finish_reason": finish_reason,
    }))
}

#[derive(Deserialize)]
struct SubmitTaskArgs {
    goal: String,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    routing: Option<String>,
}

async fn call_submit_task(client: &mut AuthedClient, args: &Value) -> Result<Value> {
    let a: SubmitTaskArgs =
        serde_json::from_value(args.clone()).context("invalid args for jarvis_submit_task")?;
    let workdir = a.workdir.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    });
    let spec = TaskSpec {
        goal: a.goal,
        workdir,
        max_steps: 0,
        sandbox: String::new(),
        net_policy: String::new(),
        use_worktree: false,
        base_ref: String::new(),
        routing_policy: a.routing.unwrap_or_default(),
        require_caps: vec![],
        parent_task_id: String::new(),
        resume_from: String::new(),
    };
    let h = client
        .submit_task(spec)
        .await
        .context("daemon SubmitTask rpc failed")?
        .into_inner();
    Ok(json!({ "task_id": h.id }))
}

#[derive(Deserialize)]
struct TaskIdArgs {
    task_id: String,
}

async fn call_get_task(client: &mut AuthedClient, args: &Value) -> Result<Value> {
    let a: TaskIdArgs =
        serde_json::from_value(args.clone()).context("invalid args for jarvis_get_task")?;
    let t = client
        .get_task(TaskHandle { id: a.task_id })
        .await
        .context("daemon GetTask rpc failed")?
        .into_inner();
    Ok(task_to_json(&t))
}

#[derive(Deserialize, Default)]
struct ListTasksArgs {
    #[serde(default)]
    all: bool,
}

async fn call_list_tasks(client: &mut AuthedClient, args: &Value) -> Result<Value> {
    let a: ListTasksArgs = if args.is_null() {
        ListTasksArgs::default()
    } else {
        serde_json::from_value(args.clone()).context("invalid args for jarvis_list_tasks")?
    };
    let resp = client
        .list_tasks(ListTasksRequest {
            include_finished: a.all,
            limit: 0,
        })
        .await
        .context("daemon ListTasks rpc failed")?
        .into_inner();
    let tasks: Vec<Value> = resp.tasks.iter().map(task_to_json).collect();
    Ok(json!({ "tasks": tasks }))
}

async fn call_cancel_task(client: &mut AuthedClient, args: &Value) -> Result<Value> {
    let a: TaskIdArgs =
        serde_json::from_value(args.clone()).context("invalid args for jarvis_cancel_task")?;
    client
        .cancel_task(TaskHandle {
            id: a.task_id.clone(),
        })
        .await
        .context("daemon CancelTask rpc failed")?;
    Ok(json!({ "cancelled": a.task_id }))
}

async fn call_status(client: &mut AuthedClient) -> Result<Value> {
    let s = client
        .get_status(StatusRequest {})
        .await
        .context("daemon GetStatus rpc failed")?
        .into_inner();
    let models: Vec<Value> = s
        .models
        .into_iter()
        .map(|m| {
            json!({
                "name": m.name,
                "kind": m.kind,
                "model_id": m.model_id,
                "priority": m.priority,
                "online": m.online,
                "quarantined": m.quarantined,
                "ctx_len": m.ctx_len,
                "tool_calls": m.tool_calls,
                "json_schema": m.json_schema,
                "vision": m.vision,
            })
        })
        .collect();
    Ok(json!({
        "version": s.version,
        "uptime_seconds": s.uptime_seconds,
        "running_tasks": s.running_tasks,
        "models": models,
    }))
}

fn task_to_json(t: &jarvis_api::Task) -> Value {
    json!({
        "id": t.id,
        "goal": t.goal,
        "status": t.status,
        "workdir": t.workdir,
        "created_at": t.created_at,
        "completed_at": t.completed_at,
        "error": t.error,
        "sandbox": t.sandbox,
        "net_policy": t.net_policy,
        "worktree_path": t.worktree_path,
        "worktree_branch": t.worktree_branch,
        "parent_task_id": t.parent_task_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_defs_contains_seven_tools() {
        let defs = tool_defs();
        assert_eq!(defs.len(), 7);
        let names: Vec<_> = defs.iter().map(|d| d.name).collect();
        assert!(names.contains(&"jarvis_ping"));
        assert!(names.contains(&"jarvis_ask"));
        assert!(names.contains(&"jarvis_submit_task"));
        assert!(names.contains(&"jarvis_get_task"));
        assert!(names.contains(&"jarvis_list_tasks"));
        assert!(names.contains(&"jarvis_cancel_task"));
        assert!(names.contains(&"jarvis_status"));
    }

    #[test]
    fn tool_defs_each_has_object_schema() {
        for d in tool_defs() {
            assert_eq!(d.input_schema["type"], "object", "tool {}", d.name);
        }
    }

    #[test]
    fn ask_args_round_trip() {
        let v = json!({ "prompt": "hi", "model": "gpt-4" });
        let a: AskArgs = serde_json::from_value(v).unwrap();
        assert_eq!(a.prompt, "hi");
        assert_eq!(a.model.as_deref(), Some("gpt-4"));
    }

    #[test]
    fn ask_args_model_optional() {
        let v = json!({ "prompt": "hi" });
        let a: AskArgs = serde_json::from_value(v).unwrap();
        assert!(a.model.is_none());
    }

    #[test]
    fn list_tasks_args_default_false() {
        let a: ListTasksArgs = serde_json::from_value(json!({})).unwrap();
        assert!(!a.all);
    }
}
