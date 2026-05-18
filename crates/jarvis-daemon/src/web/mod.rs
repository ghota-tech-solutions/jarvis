//! Embedded web UI served alongside the gRPC API.
//!
//! Borderless, OpenCode-ish aesthetic. HTMX for live updates (poll the JSON
//! endpoints, swap inner HTML). One single template file rendered with `format!`
//! macros — no template engine, no static assets, no JS framework.

use anyhow::Context as _;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use jarvis_agent::{run_agent, AgentRun};
use super::service::compile_hooks;
use jarvis_core::{clip_chars as clip, AgentId, RequiredCapabilities, TaskId, TaskKind};

mod git;
mod render;
mod sse;
mod util;
use render::{render_conversation, render_event_blocks, render_files_rollup};
use util::{
    html_escape, parse_net_policy, parse_routing, parse_sandbox_kind, parse_sandbox_mode,
    relative_time, short, short_model, urlencode, AppError,
};
use jarvis_ledger::{EventRecord, Ledger, TaskRecord, TaskRuntimeInfo, TaskStatus};
use jarvis_llm::LlmPool;
use jarvis_sandbox::{Sandbox, SandboxKind, Worktree, WorktreeManager};
use jarvis_tools::{ToolCtx, ToolRegistry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower_http::compression::CompressionLayer;
use tracing::{info, warn};

/// Shared state between axum handlers. Mirrors what the gRPC service holds —
/// the daemon is the single source of truth.
#[derive(Clone)]
pub struct WebState {
    pub pool: Arc<LlmPool>,
    pub ledger: Ledger,
    pub tools: ToolRegistry,
    pub native: Arc<dyn Sandbox>,
    pub docker: Option<Arc<dyn Sandbox>>,
    pub worktrees: Arc<WorktreeManager>,
    pub cfg: Arc<jarvis_config::Config>,
    pub started: Instant,
    pub running: Arc<Mutex<HashMap<TaskId, super::service::RuntimeHandle>>>,
    pub mcp_status: Arc<Vec<super::service::McpServerStatus>>,
}

pub async fn serve(state: WebState, addr: String) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/task/{id}", get(task_page))
        .route("/api/tasks", get(api_tasks))
        .route("/api/tasks", post(api_submit_task))
        .route("/api/tasks/{id}/cancel", post(api_cancel))
        .route("/api/events", get(api_events))
        .route("/api/events/stream", get(sse::api_events_sse))
        .route("/api/projects", get(api_projects))
        .route("/api/git", get(git::api_git))
        .route("/api/mcp", get(api_mcp))
        .route("/api/plan", get(api_plan))
        .route("/api/running", get(api_running))
        .route("/api/git/commit", post(git::api_git_commit))
        .route("/api/git/pr-url", get(git::api_git_pr_url))
        .route("/api/git/diff", get(git::api_git_diff))
        .route("/api/git/stage", post(git::api_git_stage))
        .route("/api/git/unstage", post(git::api_git_unstage))
        .route("/api/git/reset", post(git::api_git_reset))
        .route("/api/git/suggest-message", get(git::api_git_suggest_message))
        .route("/api/status", get(api_status))
        .with_state(state)
        .layer(CompressionLayer::new());

    let addr: std::net::SocketAddr = addr.parse().context("parse web.addr")?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "jarvis-web listening");
    axum::serve(listener, app)
        .await
        .context("web server")?;
    Ok(())
}

// ----------------- HTML pages -----------------

async fn index(State(s): State<WebState>) -> impl IntoResponse {
    let tasks = s
        .ledger
        .list_tasks(true, 200)
        .await
        .unwrap_or_default();
    let models = s.pool.status_all().await;
    let html = render_index(&tasks, &models, &s);
    Html(html)
}

async fn task_page(
    State(s): State<WebState>,
    Path(id): Path<String>,
) -> Result<Html<String>, AppError> {
    let task_id = TaskId::from_str(&id).map_err(|_| AppError::BadRequest("invalid task id".into()))?;
    let task = s
        .ledger
        .get_task(task_id)
        .await
        .map_err(|_| AppError::NotFound)?;
    let chain = s.ledger.walk_ancestors(task.id).await.unwrap_or_default();
    // For conversation continuity, the URL pins to the ROOT of the chain. If the
    // user landed on a child id, redirect to the root once so the path stays
    // stable across follow-ups.
    if let Some(root) = chain.first()
        && root.id != task.id
    {
        return Err(AppError::Redirect(format!("/task/{}", root.id)));
    }
    let ids: Vec<_> = chain.iter().map(|t| t.id).collect();
    let events = s
        .ledger
        .query_events_multi(&ids, 0, 0)
        .await
        .unwrap_or_default();
    let leaf = chain.last().unwrap_or(&task).clone();
    let all_tasks = s.ledger.list_tasks(true, 500).await.unwrap_or_default();
    Ok(Html(render_task_page(&task, &leaf, &chain, &events, &all_tasks)))
}

// ----------------- JSON / HTMX endpoints -----------------

#[derive(Serialize)]
struct TaskJson {
    id: String,
    parent: Option<String>,
    goal: String,
    status: String,
    workdir: String,
    sandbox: String,
    net_policy: String,
    worktree_path: String,
    worktree_branch: String,
    created_at: i64,
    completed_at: Option<i64>,
    error: Option<String>,
}

fn task_to_json(t: &TaskRecord) -> TaskJson {
    TaskJson {
        id: t.id.to_string(),
        parent: t.parent.map(|p| p.to_string()),
        goal: t.goal.clone(),
        status: t.status.to_string(),
        workdir: t.workdir.clone(),
        sandbox: t.sandbox.clone(),
        net_policy: t.net_policy.clone(),
        worktree_path: t.worktree_path.clone(),
        worktree_branch: t.worktree_branch.clone(),
        created_at: t.created_at,
        completed_at: t.completed_at,
        error: t.error.clone(),
    }
}

#[derive(Deserialize)]
struct TaskListQuery {
    #[serde(default)]
    all: bool,
    #[serde(default)]
    format: Option<String>,
}

async fn api_tasks(
    State(s): State<WebState>,
    Query(q): Query<TaskListQuery>,
) -> Response {
    let tasks = s
        .ledger
        .list_tasks(q.all, 200)
        .await
        .unwrap_or_default();
    if q.format.as_deref() == Some("html") {
        Html(render_task_rows(&tasks)).into_response()
    } else {
        let json: Vec<_> = tasks.iter().map(task_to_json).collect();
        Json(json).into_response()
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    task: String,
    #[serde(default)]
    since: i64,
    #[serde(default)]
    format: Option<String>,
}

#[derive(Serialize)]
struct EventJson {
    id: i64,
    ts_micros: i64,
    task_id: String,
    kind: String,
    subject: String,
    payload: serde_json::Value,
}

async fn api_events(
    State(s): State<WebState>,
    Query(q): Query<EventsQuery>,
) -> Result<Response, AppError> {
    let task_id =
        TaskId::from_str(&q.task).map_err(|_| AppError::BadRequest("invalid task id".into()))?;
    let chain = s.ledger.walk_ancestors(task_id).await.unwrap_or_default();
    let ids: Vec<_> = chain.iter().map(|t| t.id).collect();
    let events = s
        .ledger
        .query_events_multi(&ids, q.since, 0)
        .await
        .unwrap_or_default();
    if q.format.as_deref() == Some("html") {
        Ok(Html(render_event_blocks(&events)).into_response())
    } else {
        let json: Vec<_> = events
            .iter()
            .map(|e| EventJson {
                id: e.id.0,
                ts_micros: e.ts_micros,
                task_id: e.task_id.to_string(),
                kind: e.kind.to_string(),
                subject: e.subject.clone().unwrap_or_default(),
                payload: e.payload.clone(),
            })
            .collect();
        Ok(Json(json).into_response())
    }
}

async fn api_mcp(State(s): State<WebState>) -> Response {
    let want_html = false; // for now always JSON; sidebar HTMX uses /api/mcp?format=html below
    let _ = want_html;
    // Render compact HTML card.
    let html = render_mcp_card(&s.mcp_status);
    Html(html).into_response()
}

fn render_mcp_card(statuses: &[super::service::McpServerStatus]) -> String {
    use maud::html;
    if statuses.is_empty() {
        return html! {
            div class="git-empty" {
                div class="glyph" { "∅" }
                div class="git-empty-title" { "No MCP servers" }
                div class="muted small" {
                    "Add a "
                    code { "[mcp.servers.<name>]" }
                    " block to "
                    code { "jarvis.toml" }
                    " to import external tools."
                }
            }
        }
        .into_string();
    }
    html! {
        @for s in statuses {
            @let (sym, cls, label): (&str, &str, String) = if s.connected {
                (
                    "●",
                    "ok",
                    format!("{} tool{}", s.tools.len(), if s.tools.len() > 1 { "s" } else { "" }),
                )
            } else {
                ("✗", "err", "offline".to_string())
            };
            details class="mcp-server" {
                summary {
                    span class={ "dot " (cls) } { (sym) }
                    " "
                    b { (s.name) }
                    " "
                    span class="muted small" { (label) }
                }
                @if let Some(err) = &s.error {
                    div class="muted small" style="padding:0.2em 0.4em; color: var(--err);" { (err) }
                }
                @for tool in &s.tools {
                    div class="git-row mcp-tool" {
                        span class="git-icon" { "⚙" }
                        span class="git-row-label monoline" { (tool) }
                    }
                }
            }
        }
    }
    .into_string()
}

// ----------------- Latest plan -----------------

#[derive(Deserialize)]
struct PlanQuery {
    task: String,
}

async fn api_plan(
    State(s): State<WebState>,
    Query(q): Query<PlanQuery>,
) -> Response {
    let task_id = match TaskId::from_str(&q.task) {
        Ok(id) => id,
        Err(_) => return Html(r#"<div class="muted small">no plan yet</div>"#.to_string()).into_response(),
    };
    // Pull the conversation chain so a follow-up turn shares the parent's plan.
    let chain = collect_chain(&s.ledger, task_id).await;
    let ids: Vec<TaskId> = chain.iter().map(|t| t.id).collect();
    // Recent events across the whole chain — bounded to keep this snappy.
    let evs = s.ledger.query_events_multi(&ids, 0, 500).await.unwrap_or_default();
    let plan = evs.iter().rev().find_map(|e| {
        let is_plan = matches!(e.kind, jarvis_ledger::EventKind::ToolResult)
            && e.payload.get("tool").and_then(|v| v.as_str()) == Some("update_plan")
            && !e.payload.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
        if !is_plan {
            return None;
        }
        e.payload.get("data").and_then(|d| d.get("plan"))
            .and_then(|p| p.as_array())
            .cloned()
    });
    let html = match plan {
        Some(steps) if !steps.is_empty() => render_plan_card(&steps),
        _ => r#"<div class="muted small">no plan yet — agent has not called <code>update_plan</code></div>"#.to_string(),
    };
    Html(html).into_response()
}

async fn collect_chain(ledger: &Ledger, leaf: TaskId) -> Vec<TaskRecord> {
    let mut out: Vec<TaskRecord> = Vec::new();
    let mut cur = match ledger.get_task(leaf).await {
        Ok(t) => Some(t),
        Err(_) => return out,
    };
    let mut depth = 0;
    while let Some(t) = cur {
        let parent = t.parent;
        out.push(t);
        depth += 1;
        if depth > 64 {
            break;
        }
        cur = match parent {
            Some(pid) => ledger.get_task(pid).await.ok(),
            None => None,
        };
    }
    out
}

fn render_plan_card(steps: &[serde_json::Value]) -> String {
    use maud::html;
    let total = steps.len();
    let done = steps
        .iter()
        .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("completed"))
        .count();
    html! {
        div class="plan-progress" {
            span class="plan-count" { (done) "/" (total) } " done"
        }
        ul class="plan-list" {
            @for s in steps {
                @let text = s.get("step").and_then(|v| v.as_str()).unwrap_or("?");
                @let status = s.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                @let (cls, glyph) = match status {
                    "completed" => ("plan-step-done", "✓"),
                    "in_progress" => ("plan-step-progress", "▶"),
                    _ => ("plan-step-pending", "·"),
                };
                li class={ "plan-step " (cls) } {
                    span class="plan-glyph" { (glyph) }
                    span class="plan-text" { (text) }
                }
            }
        }
    }
    .into_string()
}

// ----------------- Running tasks live cards -----------------

#[derive(Deserialize)]
struct RunningQuery {
    #[serde(default)]
    format: Option<String>,
}

async fn api_running(
    State(s): State<WebState>,
    Query(q): Query<RunningQuery>,
) -> Response {
    let tasks = s.ledger.list_tasks(false, 50).await.unwrap_or_default();
    let running: Vec<&TaskRecord> = tasks.iter().filter(|t| t.status == TaskStatus::Running).collect();
    if q.format.as_deref() == Some("html") {
        Html(render_running_cards(&s.ledger, &running).await).into_response()
    } else {
        let json: Vec<TaskJson> = running.into_iter().map(task_to_json).collect();
        Json(json).into_response()
    }
}

async fn render_running_cards(ledger: &Ledger, running: &[&TaskRecord]) -> String {
    use maud::{html, PreEscaped};
    if running.is_empty() {
        return String::new();
    }
    // Pre-compute per-task data (async ledger calls can't run inside maud!).
    struct Row {
        id: String,
        short: String,
        elapsed: String,
        goal: String,
        step: String,
        backend: String,
        activity: String, // raw HTML — produced by `summarize_recent`
    }
    let mut rows: Vec<Row> = Vec::with_capacity(running.len());
    for t in running {
        let recent = ledger.recent_events(t.id, 6).await.unwrap_or_default();
        let (step, activity) = summarize_recent(&recent);
        rows.push(Row {
            id: t.id.to_string(),
            short: short(&t.id.to_string()),
            elapsed: relative_time(t.created_at),
            goal: clip(&t.goal, 80),
            step,
            backend: if t.sandbox.is_empty() {
                "-".to_string()
            } else if t.sandbox == "native" {
                "native".to_string()
            } else {
                format!("{}/{}", t.sandbox, t.net_policy)
            },
            activity,
        });
    }
    html! {
        div class="running-grid" {
            @for r in &rows {
                a href={ "/task/" (r.id) } class="running-card" {
                    div class="running-card-head" {
                        span class="running-dot" {}
                        span class="running-id" { (r.short) }
                        span class="running-elapsed" { (r.elapsed) }
                    }
                    div class="running-goal" { (r.goal) }
                    div class="running-meta" {
                        span class="running-step" { (r.step) }
                        span class="muted small" { (r.backend) }
                    }
                    // `activity` is a pre-rendered HTML snippet from summarize_recent.
                    div class="running-activity" { (PreEscaped(&r.activity)) }
                }
            }
        }
    }
    .into_string()
}

/// Walk the most-recent events and return (step label, latest activity html).
fn summarize_recent(events: &[EventRecord]) -> (String, String) {
    use jarvis_ledger::EventKind;
    let mut step = String::from("starting");
    let mut activity_html = String::from(r#"<span class="muted small">waiting…</span>"#);
    for ev in events.iter().rev() {
        match ev.kind {
            EventKind::Heartbeat => {
                if let Some(s) = ev.payload.get("step").and_then(|v| v.as_i64())
                    && step == "starting"
                {
                    step = format!("step {s}");
                }
            }
            EventKind::ToolCall => {
                let tool = ev.payload.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
                let args = ev.payload.get("args").cloned().unwrap_or(serde_json::Value::Null);
                let summary = match tool {
                    "shell" => format!("$ {}", args.get("cmd").and_then(|v| v.as_str()).unwrap_or("")),
                    "fs_read" | "fs_write" => format!(
                        "{} {}",
                        if tool == "fs_read" { "Read" } else { "Write" },
                        args.get("path").and_then(|v| v.as_str()).unwrap_or("")
                    ),
                    _ => tool.to_string(),
                };
                activity_html = format!(
                    r#"<span class="action-tool">→</span> <code>{}</code>"#,
                    html_escape(&clip(&summary, 80))
                );
                break;
            }
            EventKind::Decision => {
                let thought = ev.payload.get("thought").and_then(|v| v.as_str()).unwrap_or("");
                if !thought.is_empty() {
                    activity_html = format!(
                        r#"<span class="muted">{}</span>"#,
                        html_escape(&clip(thought, 100))
                    );
                    break;
                }
            }
            _ => {}
        }
    }
    (step, activity_html)
}

// ----------------- Git endpoints + rendering live in `web::git`. -----------


async fn api_status(State(s): State<WebState>) -> Response {
    let models = s.pool.status_all().await;
    let json: Vec<_> = models
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name.as_str(),
                "kind": m.kind.as_str(),
                "model_id": m.model_id,
                "priority": m.priority,
                "online": m.online,
                "quarantined": m.quarantined,
                "failures_in_window": m.failures_in_window,
            })
        })
        .collect();
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_seconds": s.started.elapsed().as_secs(),
        "models": json,
        "running_tasks": s.running.lock().await.len(),
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
struct SubmitForm {
    goal: String,
    #[serde(default)]
    workdir: String,
    #[serde(default)]
    sandbox: String,
    #[serde(default)]
    net_policy: String,
    #[serde(default)]
    routing_policy: String,
    #[serde(default)]
    sandbox_mode: String,
    #[serde(default)]
    use_worktree: Option<String>, // checkbox → "on" or absent
    #[serde(default)]
    parent_task_id: String,
}

/// HTMX-friendly submit: returns 204 (no content) when called from HTMX so the
/// page does not redirect. The caller (form on /task/{id}) updates its UI by
/// either clearing the textarea + relying on SSE/polling to surface the new
/// events. The legacy non-HTMX branch (e.g. plain HTML form on /) still
/// returns a 303 redirect to /task/{root_id}.
async fn api_submit_task(
    State(s): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(form): Form<SubmitForm>,
) -> Result<Response, AppError> {
    if form.goal.trim().is_empty() {
        return Err(AppError::BadRequest("goal is empty".into()));
    }
    let parent = if form.parent_task_id.is_empty() {
        None
    } else {
        Some(
            TaskId::from_str(&form.parent_task_id)
                .map_err(|_| AppError::BadRequest("invalid parent id".into()))?,
        )
    };
    let parent_record = match parent {
        Some(id) => Some(s.ledger.get_task(id).await.map_err(|_| AppError::NotFound)?),
        None => None,
    };

    let source_workdir = if !form.workdir.is_empty() {
        PathBuf::from(&form.workdir)
    } else if let Some(p) = &parent_record {
        PathBuf::from(&p.workdir)
    } else {
        std::env::current_dir().unwrap_or(PathBuf::from("."))
    };
    let sandbox_pref = if !form.sandbox.is_empty() {
        form.sandbox.clone()
    } else {
        parent_record.as_ref().map(|p| p.sandbox.clone()).unwrap_or_default()
    };
    let net_pref = if !form.net_policy.is_empty() {
        form.net_policy.clone()
    } else {
        parent_record.as_ref().map(|p| p.net_policy.clone()).unwrap_or_default()
    };

    let kind = parse_sandbox_kind(&sandbox_pref, &s.cfg)?;
    let net = parse_net_policy(&net_pref, &s.cfg)?;
    let sandbox = match kind {
        SandboxKind::Native => s.native.clone(),
        SandboxKind::Docker => s.docker.clone().ok_or_else(|| {
            AppError::BadRequest("docker requested but daemon could not connect".into())
        })?,
    };

    let task = s
        .ledger
        .create_task(&form.goal, &source_workdir.display().to_string(), parent_record.as_ref().map(|p| p.id))
        .await
        .map_err(|e| AppError::Internal(format!("ledger: {e}")))?;

    let use_worktree = matches!(form.use_worktree.as_deref(), Some("on" | "true"));
    let worktree = if use_worktree {
        let mgr = s.worktrees.clone();
        let task_id = task.id;
        let source = source_workdir.clone();
        match tokio::task::spawn_blocking(move || mgr.create(task_id, &source, None)).await {
            Ok(Ok(wt)) => wt,
            Ok(Err(e)) => {
                return Err(AppError::Internal(format!("worktree: {e}")));
            }
            Err(e) => return Err(AppError::Internal(format!("worktree join: {e}"))),
        }
    } else {
        Worktree {
            task_id: task.id,
            path: source_workdir.clone(),
            branch: String::new(),
            managed: false,
        }
    };

    let info = TaskRuntimeInfo {
        sandbox: kind.as_str().to_string(),
        net_policy: net.as_str().to_string(),
        worktree_path: worktree.path.display().to_string(),
        worktree_branch: worktree.branch.clone(),
    };
    let _ = s.ledger.set_task_runtime(task.id, &info).await;

    let cancel = CancellationToken::new();
    s.running.lock().await.insert(
        task.id,
        super::service::RuntimeHandle {
            cancel: cancel.clone(),
            source_workdir: source_workdir.clone(),
            worktree: worktree.clone(),
        },
    );
    let ctx = ToolCtx {
        workdir: worktree.path.clone(),
        cancel: Arc::new(cancel.clone()),
        sandbox,
        net_policy: net.clone(),
    };
    let routing = parse_routing(&form.routing_policy, &s.cfg);
    let run = AgentRun {
        task_id: task.id,
        workdir: worktree.path.clone(),
        max_steps: 20,
        agent_id: AgentId::new(),
        cancel,
        routing,
        required: RequiredCapabilities::default(),
        kind: TaskKind::Planning,
        continuation_budget: 1,
        hooks: compile_hooks(&s.cfg.hooks),
        sandbox_mode: parse_sandbox_mode(&form.sandbox_mode, &s.cfg),
    };
    let pool = s.pool.clone();
    let ledger = s.ledger.clone();
    let tools = s.tools.clone();
    let running = s.running.clone();
    let worktrees = s.worktrees.clone();
    let task_id = task.id;
    tokio::spawn(async move {
        let outcome = run_agent(run, pool, ledger, tools, ctx).await;
        match &outcome {
            Ok(o) => info!(task = %task_id, ?o, "web: agent finished"),
            Err(e) => warn!(task = %task_id, error = %e, "web: agent failed"),
        }
        let handle = { running.lock().await.remove(&task_id) };
        if let Some(h) = handle
            && h.worktree.managed
        {
            let wt = h.worktree.clone();
            let source = h.source_workdir.clone();
            let mgr = worktrees.clone();
            let _ = tokio::task::spawn_blocking(move || mgr.cleanup(&wt, &source)).await;
        }
    });

    // Resolve the ROOT of the chain — that's the stable URL for the conversation.
    let root_id = match parent_record {
        Some(p) => {
            let chain = s
                .ledger
                .walk_ancestors(p.id)
                .await
                .unwrap_or_default();
            chain.first().map(|r| r.id).unwrap_or(p.id)
        }
        None => task.id,
    };

    let is_htmx = headers
        .get("HX-Request")
        .map(|v| v.to_str().unwrap_or("").eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if is_htmx {
        // Stay on the current page; SSE / polling will deliver the new events.
        // We trigger a custom event so the form can react (e.g. clear textarea).
        Ok((
            StatusCode::NO_CONTENT,
            [("HX-Trigger", "jarvis-task-submitted")],
        )
            .into_response())
    } else {
        Ok((
            StatusCode::SEE_OTHER,
            [
                ("HX-Redirect", format!("/task/{root_id}")),
                ("Location", format!("/task/{root_id}")),
            ],
        )
            .into_response())
    }
}

// ----------------- Projects + Git -----------------

#[derive(Serialize)]
struct ProjectGroup {
    workdir: String,
    short: String,
    task_count: usize,
    last_active_micros: i64,
    tasks: Vec<TaskJson>,
}

async fn api_projects(State(s): State<WebState>) -> Response {
    let tasks = s.ledger.list_tasks(true, 500).await.unwrap_or_default();
    let mut by_dir: std::collections::HashMap<String, Vec<TaskRecord>> = Default::default();
    for t in tasks {
        by_dir.entry(t.workdir.clone()).or_default().push(t);
    }
    let mut groups: Vec<ProjectGroup> = by_dir
        .into_iter()
        .map(|(workdir, tasks)| {
            let last = tasks.iter().map(|t| t.created_at).max().unwrap_or(0);
            let short = workdir
                .rsplit_once(['/', '\\'])
                .map(|(_, n)| n.to_string())
                .unwrap_or_else(|| workdir.clone());
            let task_count = tasks.len();
            let tasks_json: Vec<_> = tasks.iter().map(task_to_json).collect();
            ProjectGroup {
                workdir,
                short,
                task_count,
                last_active_micros: last,
                tasks: tasks_json,
            }
        })
        .collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.last_active_micros));
    Json(groups).into_response()
}


async fn api_cancel(
    State(s): State<WebState>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let task_id = TaskId::from_str(&id).map_err(|_| AppError::BadRequest("invalid id".into()))?;
    let tok = s.running.lock().await.get(&task_id).map(|h| h.cancel.clone());
    match tok {
        Some(c) => {
            c.cancel();
            Ok((StatusCode::NO_CONTENT, "").into_response())
        }
        None => Err(AppError::NotFound),
    }
}

// (helpers + AppError live in `web::util`)

// ----------------- templates -----------------

fn render_index(
    tasks: &[TaskRecord],
    models: &[jarvis_llm::ModelStatus],
    s: &WebState,
) -> String {
    let project_groups = group_by_workdir(tasks);
    let projects_html = render_projects_nav(&project_groups, None);
    let model_rows = render_model_rows(models);
    let task_rows = render_task_rows(tasks);
    let uptime = s.started.elapsed().as_secs();
    format!(
        r##"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>jarvis · dashboard</title>
<style>{CSS}</style>
<script src="https://unpkg.com/htmx.org@2.0.3" integrity="sha384-0895/pl2MU10Hqc6jd4RvrthNlDiE9U1tWmX7WRESftEDRosgxNsQG/Ze9YMRzHq" crossorigin="anonymous"></script>
</head>
<body>
<div class="shell">
  <nav class="left">
    <div class="brand">jarvis</div>
    <a href="/" class="navitem active">⌂ Dashboard</a>
    <a href="#new-task" class="navitem">＋ New task</a>
    <div class="section-label">Projects</div>
    {projects_html}
    <div class="navfoot muted small">daemon v{ver} · {uptime}s</div>
  </nav>
  <main class="main">
    <header class="task-header">
      <h1>Tasks</h1>
      <p class="muted">{n_tasks} task(s) · daemon up {uptime}s</p>
    </header>
    <section class="running-section"
             hx-get="/api/running?format=html"
             hx-trigger="load, every 2s"
             hx-target="this"
             hx-swap="innerHTML">
    </section>
    <section id="new-task" class="card">
      <h2>New task</h2>
      <form hx-post="/api/tasks" hx-encoding="application/x-www-form-urlencoded" class="newtask" hx-on::after-request="if (event.detail.successful) this.reset()">
        <textarea name="goal" rows="2" placeholder="Describe the goal — Enter to submit, Shift+Enter for a newline" required></textarea>
        <div class="row">
          <input name="workdir" placeholder="workdir (leave empty for cwd)">
          <select name="sandbox"><option value="">sandbox: default</option><option>native</option><option>docker</option></select>
          <select name="net_policy"><option value="">net: default</option><option>none</option><option>egress_only</option><option>full</option></select>
          <select name="routing_policy"><option value="">routing: default</option><option>auto</option><option>local_only</option><option>remote_only</option></select>
          <select name="sandbox_mode" title="What the agent is allowed to do"><option value="">mode: default</option><option value="read_only">read-only</option><option value="workspace_write">workspace-write</option><option value="danger_full_access">danger-full-access</option></select>
          <label class="checkbox"><input type="checkbox" name="use_worktree"> worktree</label>
          <button type="submit">submit</button>
        </div>
      </form>
    </section>
    <script>{dashboard_js}</script>
    <table class="tasks">
      <thead><tr><th>id</th><th>status</th><th>backend</th><th>goal</th><th>turns</th><th>updated</th></tr></thead>
      <tbody hx-get="/api/tasks?all=true&format=html" hx-trigger="every 2s" hx-target="this" hx-swap="innerHTML">{task_rows}</tbody>
    </table>
  </main>
  <aside class="right">
    <div class="section-label">Models</div>
    <div class="models">{model_rows}</div>
  </aside>
</div>
</body></html>"##,
        ver = env!("CARGO_PKG_VERSION"),
        uptime = uptime,
        n_tasks = tasks.len(),
        projects_html = projects_html,
        task_rows = task_rows,
        model_rows = model_rows,
        dashboard_js = DASHBOARD_JS,
    )
}

fn group_by_workdir(tasks: &[TaskRecord]) -> Vec<(String, Vec<&TaskRecord>)> {
    let mut by_dir: std::collections::HashMap<String, Vec<&TaskRecord>> = Default::default();
    for t in tasks {
        by_dir.entry(t.workdir.clone()).or_default().push(t);
    }
    let mut groups: Vec<(String, Vec<&TaskRecord>)> = by_dir.into_iter().collect();
    for (_, v) in groups.iter_mut() {
        v.sort_by_key(|t| std::cmp::Reverse(t.created_at));
    }
    groups.sort_by_key(|(_, v)| std::cmp::Reverse(v.first().map(|t| t.created_at).unwrap_or(0)));
    groups
}

fn render_projects_nav(
    groups: &[(String, Vec<&TaskRecord>)],
    current_workdir: Option<&str>,
) -> String {
    use maud::html;
    if groups.is_empty() {
        return html! { div class="muted small" { "no projects yet" } }.into_string();
    }
    html! {
        @for (workdir, tasks) in groups {
            @let short_name = workdir
                .rsplit_once(['/', '\\'])
                .map(|(_, n)| n.to_string())
                .unwrap_or_else(|| workdir.clone());
            @let active = current_workdir.map(|w| w == workdir.as_str()).unwrap_or(false);
            @let roots: Vec<_> = tasks.iter().filter(|t| t.parent.is_none()).collect();
            details class="project" open[active] {
                summary {
                    "📁 " (short_name) " "
                    span class="muted small" { (roots.len()) }
                }
                // Each root is one conversation. Children show up rendered inside the
                // conversation via the ancestor chain.
                @for t in roots.iter().take(12) {
                    @let status = t.status.to_string();
                    a href={ "/task/" (t.id) } class={ "project-task t-" (status) } {
                        span class="goal" { (clip(&t.goal, 64)) }
                        span class="ago muted small" { (relative_time(t.created_at)) }
                    }
                }
            }
        }
    }
    .into_string()
}

/// One row per conversation: the root's goal heads the row, the leaf's status
/// is the displayed status, and a turn count shows how many follow-ups live in
/// the chain. Mirrors the way the projects nav and detail page see the data.
fn render_task_rows(tasks: &[TaskRecord]) -> String {
    use maud::html;
    let conversations = group_into_conversations(tasks);
    if conversations.is_empty() {
        return html! { tr { td colspan="6" class="muted" { "no tasks yet" } } }.into_string();
    }
    html! {
        @for c in &conversations {
            @let backend = if c.leaf_sandbox.is_empty() {
                "-".to_string()
            } else if c.leaf_sandbox == "native" {
                "native".to_string()
            } else {
                format!("{}/{}", c.leaf_sandbox, c.leaf_net_policy)
            };
            tr class={ "t-" (c.leaf_status) } {
                td { a href={ "/task/" (c.root_id) } { (short(&c.root_id.to_string())) } }
                td { (c.leaf_status) }
                td { (backend) }
                td { (clip(&c.root_goal, 120)) }
                td {
                    @if c.turn_count > 1 {
                        span class="turns-badge" { (c.turn_count) }
                    } @else {
                        span class="muted small" { "1" }
                    }
                }
                td class="muted" { (relative_time(c.updated_at)) }
            }
        }
    }
    .into_string()
}

#[derive(Debug)]
struct ConversationRow {
    root_id: TaskId,
    root_goal: String,
    leaf_status: String,
    leaf_sandbox: String,
    leaf_net_policy: String,
    turn_count: usize,
    updated_at: i64,
}

fn group_into_conversations(tasks: &[TaskRecord]) -> Vec<ConversationRow> {
    use std::collections::HashMap;
    if tasks.is_empty() {
        return Vec::new();
    }
    let by_id: HashMap<TaskId, &TaskRecord> = tasks.iter().map(|t| (t.id, t)).collect();
    fn root_of<'a>(start: &'a TaskRecord, by_id: &HashMap<TaskId, &'a TaskRecord>) -> &'a TaskRecord {
        let mut current = start;
        let mut depth = 0;
        while let Some(parent_id) = current.parent {
            depth += 1;
            if depth > 64 {
                break;
            }
            match by_id.get(&parent_id) {
                Some(p) => current = p,
                None => break,
            }
        }
        current
    }
    let mut by_root: HashMap<TaskId, Vec<&TaskRecord>> = HashMap::new();
    for t in tasks {
        let r = root_of(t, &by_id);
        by_root.entry(r.id).or_default().push(t);
    }
    let mut rows: Vec<ConversationRow> = by_root
        .into_iter()
        .filter_map(|(root_id, members)| {
            let root = by_id.get(&root_id)?;
            let leaf = members
                .iter()
                .max_by_key(|t| t.created_at)
                .copied()
                .unwrap_or(root);
            Some(ConversationRow {
                root_id,
                root_goal: root.goal.clone(),
                leaf_status: leaf.status.to_string(),
                leaf_sandbox: leaf.sandbox.clone(),
                leaf_net_policy: leaf.net_policy.clone(),
                turn_count: members.len(),
                updated_at: leaf.created_at,
            })
        })
        .collect();
    // Running conversations rise to the top so the dashboard always shows
    // active work first; otherwise sort by recency.
    rows.sort_by(|a, b| {
        let a_running = a.leaf_status == "running";
        let b_running = b.leaf_status == "running";
        b_running
            .cmp(&a_running)
            .then(b.updated_at.cmp(&a.updated_at))
    });
    rows
}

fn render_model_rows(models: &[jarvis_llm::ModelStatus]) -> String {
    use maud::html;
    html! {
        @for m in models {
            @let (sym, cls) = if !m.online {
                ("·", "off")
            } else if m.quarantined {
                ("✗", "err")
            } else {
                ("●", "ok")
            };
            div class="model" {
                span class={ "dot " (cls) } { (sym) }
                " " (short_model(m.name.as_str())) " "
                span class="muted" { "· " (m.kind.as_str()) }
            }
        }
    }
    .into_string()
}

fn render_task_page(
    root: &TaskRecord,
    leaf: &TaskRecord,
    chain: &[TaskRecord],
    events: &[EventRecord],
    all_tasks: &[TaskRecord],
) -> String {
    // Group events by task_id, then render each task as a (user message + assistant
    // events) turn. The first task in the chain doesn't get a "user" block because
    // its goal is already the page header.
    let blocks = render_conversation(chain, events);
    let files_rollup = render_files_rollup(events);
    let last_id = events.last().map(|e| e.id.0).unwrap_or(0);
    // Project nav (left) — re-rendered with the current workdir highlighted.
    let projects = group_by_workdir(all_tasks);
    let projects_html = render_projects_nav(&projects, Some(&root.workdir));
    format!(
        r##"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>jarvis · {short}</title>
<style>{CSS}</style>
<script src="https://unpkg.com/htmx.org@2.0.3" integrity="sha384-0895/pl2MU10Hqc6jd4RvrthNlDiE9U1tWmX7WRESftEDRosgxNsQG/Ze9YMRzHq" crossorigin="anonymous"></script>
<script src="https://unpkg.com/htmx-ext-sse@2.2.2" crossorigin="anonymous"></script>
</head>
<body>
<div class="shell">
  <nav class="left">
    <div class="brand">jarvis</div>
    <a href="/" class="navitem">⌂ Dashboard</a>
    <a href="#continue" class="navitem">＋ Continue</a>
    <div class="section-label">Projects</div>
    {projects_html}
  </nav>
  <main class="main">
    <header class="task-header">
      <h1>{goal}<span class="status-pill s-{status}"><span class="dot"></span>{status}</span></h1>
      <p class="muted small">{backend} · {short_id} {chain_label_inline}</p>
    </header>
    <div class="events"
         id="events"
         hx-ext="sse"
         sse-connect="/api/events/stream?task={root_id}&since={last_id}"
         sse-swap="event"
         hx-swap="beforeend">
      {blocks}
      {empty_state}
      <div id="composing" class="turn assistant composing" hidden></div>
    </div>
    <div id="diff-fullscreen" class="diff-fullscreen" hidden>
      <div class="diff-fullscreen-head">
        <button type="button" class="diff-back" onclick="jarvisCloseFullDiff()" title="Back (Esc)">← back</button>
        <strong id="diff-fullscreen-title">Diff preview</strong>
        <span class="muted small">press Esc to return</span>
      </div>
      <div id="diff-fullscreen-body" class="diff-fullscreen-body">
        <div class="muted small">loading…</div>
      </div>
    </div>
    <button class="jump-bottom" id="jump-bottom">↓ <span id="jump-count">new</span></button>
    {files_rollup}
    <form id="continue" class="continue"
          hx-post="/api/tasks"
          hx-headers='{{"HX-Request": "true"}}'
          hx-swap="none"
          hx-on::after-request="if (event.detail.successful) this.reset()">
      <input type="hidden" name="parent_task_id" value="{leaf_id}">
      <textarea name="goal" rows="2" placeholder="Ask for follow-up changes — Enter to send, Shift+Enter for newline" required></textarea>
      <div class="continue-actions">
        <button type="submit" title="Send">↑ send</button>
      </div>
    </form>
  </main>
  <aside class="right">
    <div class="section-label">Git</div>
    <div id="git-card" class="git-card"
         hx-get="/api/git?workdir={workdir_q}"
         hx-trigger="load, every 8s"
         hx-target="this"
         hx-swap="innerHTML">
      <div class="muted small">loading…</div>
    </div>
    <div class="section-label">MCP</div>
    <div id="mcp-card" class="git-card"
         hx-get="/api/mcp"
         hx-trigger="load, every 30s"
         hx-target="this"
         hx-swap="innerHTML">
      <div class="muted small">loading…</div>
    </div>
    <div class="section-label">Plan</div>
    <div id="plan-card" class="plan-card"
         hx-get="/api/plan?task={root_id}"
         hx-trigger="load, every 4s"
         hx-target="this"
         hx-swap="innerHTML">
      <div class="muted small">no plan yet</div>
    </div>
    <div class="section-label">Task</div>
    <div class="kv"><span>id</span><b>{short_id}</b></div>
    <div class="kv"><span>status</span><b class="s-{status}">{status}</b></div>
    {sandbox_kv}
    <div class="section-label">Workdir</div>
    <div class="muted small monoline">{workdir}</div>
    {worktree_section}
    <div class="section-label">Actions</div>
    <button type="button" class="action-primary" onclick="jarvisContinue()" title="Force the agent to continue working until the original goal is verifiably done">▶ Continue working</button>
    <button class="ghost" hx-post="/api/tasks/{leaf_id}/cancel" hx-confirm="Cancel this task?">cancel current</button>
    <div class="section-label">Diff preview</div>
    <div class="muted small" style="padding: 0.3em 0.4em;">click a changed file ↑ — diff opens in the main panel (Esc to return)</div>
  </aside>
</div>
<script>{task_js}</script>
<div id="git-commit-modal" class="modal" onclick="if (event.target === this) jarvisCloseCommitModal();">
  <div class="modal-card">
    <div class="modal-head">
      <strong>Commit changes</strong>
      <button type="button" class="modal-close" onclick="jarvisCloseCommitModal()">×</button>
    </div>
    <form id="git-commit-form" hx-post="/api/git/commit"
          hx-headers='{{"HX-Request": "true"}}'
          hx-target="#git-commit-status"
          hx-swap="innerHTML"
          hx-on::after-request="if (event.detail.successful) this.reset()">
      <input type="hidden" name="workdir" id="git-commit-workdir" value="">
      <div class="modal-msg-row">
        <textarea name="message" id="git-commit-message"
                  rows="5"
                  placeholder="commit message — short subject, then a blank line for the body"
                  required></textarea>
        <button type="button" class="ghost suggest-btn"
                title="Generate a commit message from the diff using the LLM"
                onclick="jarvisSuggestCommitMessage()">✨ Suggest</button>
      </div>
      <label class="modal-checkbox">
        <input type="checkbox" name="stage_all" id="git-commit-stage-all" checked>
        Stage all changes before committing (git commit -a)
      </label>
      <div class="modal-actions">
        <span id="git-commit-status" class="muted small"></span>
        <button type="button" class="ghost" onclick="jarvisCloseCommitModal()">cancel</button>
        <button type="submit">Commit</button>
      </div>
    </form>
  </div>
</div>
</body></html>"##,
        short = short(&root.id.to_string()),
        short_id = short(&root.id.to_string()),
        goal = html_escape(&root.goal),
        status = html_escape(&leaf.status.to_string()),
        backend = html_escape(&{
            if leaf.sandbox.is_empty() {
                "-".to_string()
            } else {
                format!("{}/{}", leaf.sandbox, leaf.net_policy)
            }
        }),
        root_id = root.id,
        leaf_id = leaf.id,
        last_id = last_id,
        chain_label_inline = if chain.len() > 1 {
            format!("· {} turns", chain.len())
        } else {
            String::new()
        },
        blocks = blocks,
        empty_state = if events.is_empty() {
            r#"<div class="turn empty-state"><div class="glyph">○</div><div>Jarvis is starting…</div><div class="muted small">The first turn will appear here.</div></div>"#.to_string()
        } else {
            String::new()
        },
        projects_html = projects_html,
        sandbox_kv = if leaf.sandbox.is_empty() {
            String::new()
        } else {
            format!(
                r#"<div class="section-label">Sandbox</div><div class="kv"><span>backend</span><b>{}</b></div><div class="kv"><span>network</span><b>{}</b></div>"#,
                html_escape(&leaf.sandbox),
                html_escape(&leaf.net_policy)
            )
        },
        workdir = html_escape(&root.workdir),
        workdir_q = urlencode(&root.workdir),
        worktree_section = if leaf.worktree_path.is_empty() || leaf.worktree_path == leaf.workdir {
            String::new()
        } else {
            format!(
                r#"<div class="section-label">Worktree</div><div class="muted small monoline">{}</div><div class="kv"><span>branch</span><b>{}</b></div>"#,
                html_escape(&leaf.worktree_path),
                html_escape(&leaf.worktree_branch),
            )
        },
        task_js = TASK_JS,
    )
}

const CSS: &str = include_str!("../../assets/web.css");
const TASK_JS: &str = include_str!("../../assets/web-task.js");
const DASHBOARD_JS: &str = include_str!("../../assets/web-dashboard.js");
// Tests for git helpers live alongside their implementation in `web::git`.
