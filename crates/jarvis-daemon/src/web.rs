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
use jarvis_core::{AgentId, RequiredCapabilities, RoutingPolicy, TaskId, TaskKind};
use jarvis_ledger::{EventRecord, Ledger, TaskRecord, TaskRuntimeInfo};
use jarvis_llm::LlmPool;
use jarvis_sandbox::{NetPolicy, Sandbox, SandboxKind, Worktree, WorktreeManager};
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
}

pub async fn serve(state: WebState, addr: String) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/task/{id}", get(task_page))
        .route("/api/tasks", get(api_tasks))
        .route("/api/tasks", post(api_submit_task))
        .route("/api/tasks/{id}/cancel", post(api_cancel))
        .route("/api/events", get(api_events))
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
    let ids: Vec<_> = chain.iter().map(|t| t.id).collect();
    let events = s
        .ledger
        .query_events_multi(&ids, 0, 0)
        .await
        .unwrap_or_default();
    Ok(Html(render_task_page(&task, &chain, &events, &s)))
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
    use_worktree: Option<String>, // checkbox → "on" or absent
    #[serde(default)]
    parent_task_id: String,
}

async fn api_submit_task(
    State(s): State<WebState>,
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

    // HTMX redirect — the dashboard refreshes after submission.
    Ok((
        StatusCode::SEE_OTHER,
        [("HX-Redirect", format!("/task/{}", task.id)), ("Location", format!("/task/{}", task.id))],
    )
        .into_response())
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

// ----------------- helpers -----------------

fn parse_sandbox_kind(s: &str, cfg: &jarvis_config::Config) -> Result<SandboxKind, AppError> {
    let s = if s.is_empty() { cfg.sandbox.default_backend.as_str() } else { s };
    SandboxKind::from_str(s).map_err(AppError::BadRequest)
}

fn parse_net_policy(s: &str, cfg: &jarvis_config::Config) -> Result<NetPolicy, AppError> {
    let s = if s.is_empty() { cfg.sandbox.default_net_policy.as_str() } else { s };
    NetPolicy::from_str(s).map_err(AppError::BadRequest)
}

fn parse_routing(s: &str, cfg: &jarvis_config::Config) -> RoutingPolicy {
    let s = if s.is_empty() { cfg.routing.default_policy.as_str() } else { s };
    super::service::parse_routing_str(s).unwrap_or(RoutingPolicy::Auto)
}

// ----------------- error -----------------

#[derive(Debug)]
enum AppError {
    BadRequest(String),
    NotFound,
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            AppError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, msg).into_response()
    }
}

// ----------------- templates -----------------

fn render_index(
    tasks: &[TaskRecord],
    models: &[jarvis_llm::ModelStatus],
    s: &WebState,
) -> String {
    let task_rows = render_task_rows(tasks);
    let model_rows = render_model_rows(models);
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
<div class="layout">
  <main class="main">
    <h1>jarvis</h1>
    <p class="muted">daemon v{ver} · up {uptime}s · {n_tasks} tasks visible</p>

    <h2>New task</h2>
    <form hx-post="/api/tasks" hx-encoding="application/x-www-form-urlencoded" class="newtask">
      <textarea name="goal" rows="2" placeholder="Describe the goal — Enter to submit" required></textarea>
      <div class="row">
        <input name="workdir" placeholder="workdir (leave empty for cwd)">
        <select name="sandbox"><option value="">sandbox: default</option><option>native</option><option>docker</option></select>
        <select name="net_policy"><option value="">net: default</option><option>none</option><option>egress_only</option><option>full</option></select>
        <select name="routing_policy"><option value="">routing: default</option><option>auto</option><option>local_only</option><option>remote_only</option></select>
        <label class="checkbox"><input type="checkbox" name="use_worktree"> worktree</label>
        <button type="submit">submit</button>
      </div>
    </form>

    <h2>Tasks</h2>
    <table class="tasks" hx-get="/api/tasks?all=true&format=html" hx-trigger="every 2s" hx-target="this" hx-swap="outerHTML">
      <thead><tr><th>id</th><th>status</th><th>sandbox</th><th>goal</th><th>created</th></tr></thead>
      <tbody>{task_rows}</tbody>
    </table>
  </main>
  <aside class="sidebar">
    <h3>▼ Models</h3>
    <div class="models">{model_rows}</div>
  </aside>
</div>
</body></html>"##,
        ver = env!("CARGO_PKG_VERSION"),
        uptime = uptime,
        n_tasks = tasks.len(),
        task_rows = task_rows,
        model_rows = model_rows,
    )
}

fn render_task_rows(tasks: &[TaskRecord]) -> String {
    let mut s = String::new();
    if tasks.is_empty() {
        s.push_str(r#"<tr><td colspan="5" class="muted">no tasks yet</td></tr>"#);
        return s;
    }
    for t in tasks {
        let backend = if t.sandbox.is_empty() {
            String::from("-")
        } else if t.sandbox == "native" {
            "native".to_string()
        } else {
            format!("{}/{}", t.sandbox, t.net_policy)
        };
        let created = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(t.created_at)
            .map(|d| d.format("%H:%M:%S").to_string())
            .unwrap_or_default();
        s.push_str(&format!(
            r#"<tr class="t-{status}"><td><a href="/task/{id}">{short}</a></td><td>{status}</td><td>{backend}</td><td>{goal}</td><td class="muted">{created}</td></tr>"#,
            id = t.id,
            short = short(&t.id.to_string()),
            status = html_escape(&t.status.to_string()),
            backend = html_escape(&backend),
            goal = html_escape(&clip(&t.goal, 120)),
            created = created,
        ));
    }
    s
}

fn render_model_rows(models: &[jarvis_llm::ModelStatus]) -> String {
    let mut s = String::new();
    for m in models {
        let (sym, cls) = if !m.online {
            ("·", "off")
        } else if m.quarantined {
            ("✗", "err")
        } else {
            ("●", "ok")
        };
        s.push_str(&format!(
            r#"<div class="model"><span class="dot {cls}">{sym}</span> {name} <span class="muted">· {kind}</span></div>"#,
            name = html_escape(&short_model(m.name.as_str())),
            kind = html_escape(m.kind.as_str()),
        ));
    }
    s
}

fn render_task_page(task: &TaskRecord, chain: &[TaskRecord], events: &[EventRecord], _s: &WebState) -> String {
    let blocks = render_event_blocks(events);
    let chain_label = if chain.len() > 1 {
        format!(r#"<p class="muted">conversation chain · {} task(s)</p>"#, chain.len())
    } else {
        String::new()
    };
    let parent_marker = if task.parent.is_some() {
        r#"<span class="badge">continuation</span>"#
    } else {
        ""
    };
    let last_id = events.last().map(|e| e.id.0).unwrap_or(0);
    format!(
        r##"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>jarvis · {short}</title>
<style>{CSS}</style>
<script src="https://unpkg.com/htmx.org@2.0.3" integrity="sha384-0895/pl2MU10Hqc6jd4RvrthNlDiE9U1tWmX7WRESftEDRosgxNsQG/Ze9YMRzHq" crossorigin="anonymous"></script>
</head>
<body>
<div class="layout">
  <main class="main">
    <p><a href="/">← all tasks</a></p>
    <h1>{goal} {parent_marker}</h1>
    <p class="muted">{status} · {backend} · {id}</p>
    {chain_label}
    <div class="events" id="events" hx-get="/api/events?task={id}&since={last_id}&format=html" hx-trigger="every 1.5s" hx-swap="beforeend">
      {blocks}
    </div>
    <form hx-post="/api/tasks" class="continue">
      <input type="hidden" name="parent_task_id" value="{id}">
      <textarea name="goal" rows="2" placeholder="Continue the conversation… (Enter)"></textarea>
      <button type="submit">send</button>
    </form>
  </main>
  <aside class="sidebar">
    <h3>▼ Task</h3>
    <div class="kv"><span>id</span><b>{short}</b></div>
    <div class="kv"><span>status</span><b class="s-{status}">{status}</b></div>
    {sandbox_kv}
    <h3>▼ Workdir</h3>
    <div class="muted small">{workdir}</div>
    {worktree_section}
    <h3>▼ Actions</h3>
    <button hx-post="/api/tasks/{id}/cancel" hx-confirm="Cancel this task?">cancel</button>
  </aside>
</div>
</body></html>"##,
        short = short(&task.id.to_string()),
        goal = html_escape(&task.goal),
        parent_marker = parent_marker,
        status = html_escape(&task.status.to_string()),
        backend = html_escape(&{
            if task.sandbox.is_empty() {
                "-".to_string()
            } else {
                format!("{}/{}", task.sandbox, task.net_policy)
            }
        }),
        id = task.id,
        last_id = last_id,
        chain_label = chain_label,
        blocks = blocks,
        sandbox_kv = if task.sandbox.is_empty() {
            String::new()
        } else {
            format!(
                r#"<h3>▼ Sandbox</h3><div class="kv"><span>backend</span><b>{}</b></div><div class="kv"><span>network</span><b>{}</b></div>"#,
                html_escape(&task.sandbox),
                html_escape(&task.net_policy)
            )
        },
        workdir = html_escape(&task.workdir),
        worktree_section = if task.worktree_path.is_empty() || task.worktree_path == task.workdir {
            String::new()
        } else {
            format!(
                r#"<h3>▼ Worktree</h3><div class="muted small">{}</div><div class="kv"><span>branch</span><b>{}</b></div>"#,
                html_escape(&task.worktree_path),
                html_escape(&task.worktree_branch),
            )
        },
    )
}

fn render_event_blocks(events: &[EventRecord]) -> String {
    let mut s = String::new();
    for ev in events {
        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(ev.ts_micros)
            .map(|d| d.format("%H:%M:%S").to_string())
            .unwrap_or_default();
        let kind = ev.kind.to_string();
        let body = match kind.as_str() {
            "decision" => ev
                .payload
                .get("thought")
                .and_then(|v| v.as_str())
                .map(html_escape)
                .unwrap_or_default(),
            "tool_call" => {
                let tool = ev.payload.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
                let args = ev.payload.get("args").map(|a| a.to_string()).unwrap_or_default();
                format!(
                    r#"<span class="tool">→ {}</span> <span class="args">{}</span>"#,
                    html_escape(tool),
                    html_escape(&clip(&args, 200))
                )
            }
            "tool_result" => {
                let summary = ev
                    .payload
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let is_error = ev
                    .payload
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let glyph = if is_error { r#"<span class="err">✗</span>"# } else { r#"<span class="ok">✓</span>"# };
                format!("{glyph} {}", html_escape(summary))
            }
            "error" => {
                let msg = ev
                    .payload
                    .get("message")
                    .or_else(|| ev.payload.get("error"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!(r#"<span class="err">✗ {}</span>"#, html_escape(msg))
            }
            "verdict" => {
                let v = ev.payload.get("verdict").and_then(|x| x.as_str()).unwrap_or("?");
                let m = ev.payload.get("message").and_then(|x| x.as_str()).unwrap_or("");
                let cls = match v {
                    "pass" => "ok",
                    "fail" => "err",
                    _ => "warn",
                };
                format!(r#"<span class="{cls}">■ {v}</span> {}"#, html_escape(m))
            }
            "heartbeat" => {
                let step = ev.payload.get("step").and_then(|v| v.as_i64()).unwrap_or(0);
                format!(r#"<span class="muted small">─ step {step} ─</span>"#)
            }
            _ => html_escape(&ev.payload.to_string()),
        };
        if body.is_empty() {
            continue;
        }
        s.push_str(&format!(
            r#"<div class="evt evt-{kind}" data-id="{id}"><span class="ts muted">{ts}</span><span class="kind muted">{kind}</span><span class="body">{body}</span></div>"#,
            id = ev.id.0,
            kind = kind,
        ));
    }
    s
}

fn short(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_string()
}
fn short_model(name: &str) -> String {
    name.split_once(':').map(|(_, n)| n.to_string()).unwrap_or_else(|| name.to_string())
}
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const CSS: &str = r#"
:root {
  --bg: #0e0e10;
  --fg: #c4c4c4;
  --dim: #808080;
  --fade: #505050;
  --heading: #e6dcc8;
  --accent: #d4b478;
  --ok: #8cb46e;
  --err: #dc6e5a;
  --warn: #dcaf5a;
  --assistant: #b4c8dc;
  --link: #d4b478;
}
* { box-sizing: border-box; }
html, body { background: var(--bg); color: var(--fg); margin: 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; line-height: 1.55; }
a { color: var(--link); text-decoration: none; }
a:hover { text-decoration: underline; }
h1 { font-size: 18px; font-weight: 600; color: var(--heading); margin: 1em 0 0.4em; }
h2 { font-size: 14px; color: var(--heading); margin: 1.6em 0 0.5em; font-weight: 600; }
h3 { font-size: 12px; color: var(--heading); margin: 1.2em 0 0.5em; font-weight: 600; text-transform: none; }
p { margin: 0.3em 0; }
.muted { color: var(--dim); }
.small { font-size: 12px; }
.layout { display: grid; grid-template-columns: 1fr 280px; min-height: 100vh; max-width: 1400px; margin: 0 auto; padding: 1em 1.5em; gap: 1.5em; }
.main { min-width: 0; }
.sidebar { border-left: 1px solid #1c1c1f; padding-left: 1.2em; }
table.tasks { width: 100%; border-collapse: collapse; font-size: 13px; }
table.tasks th { text-align: left; color: var(--dim); font-weight: 400; padding: 0.3em 0.5em; border-bottom: 1px solid #1c1c1f; }
table.tasks td { padding: 0.3em 0.5em; vertical-align: top; }
table.tasks tr:hover td { background: #15151a; }
tr.t-completed td:nth-child(2) { color: var(--ok); }
tr.t-running td:nth-child(2) { color: var(--warn); }
tr.t-failed td:nth-child(2) { color: var(--err); }
tr.t-cancelled td:nth-child(2) { color: var(--fade); }
form.newtask textarea, form.continue textarea { width: 100%; background: #15151a; color: var(--fg); border: 1px solid #2a2a30; padding: 0.5em 0.6em; font-family: inherit; font-size: 13px; resize: vertical; }
form .row { display: flex; gap: 0.5em; margin-top: 0.5em; flex-wrap: wrap; align-items: center; }
form input[type=text], form input:not([type]), form select { background: #15151a; color: var(--fg); border: 1px solid #2a2a30; padding: 0.35em 0.5em; font-family: inherit; font-size: 12px; }
form button { background: var(--accent); color: #1a1408; border: none; padding: 0.45em 1em; font-family: inherit; font-size: 12px; font-weight: 600; cursor: pointer; }
form button:hover { background: #e0c890; }
.checkbox { display: flex; gap: 0.3em; align-items: center; color: var(--dim); font-size: 12px; }
.events { margin: 0.6em 0; }
.evt { display: grid; grid-template-columns: 70px 110px 1fr; gap: 0.5em; padding: 0.15em 0.2em; align-items: baseline; }
.evt:hover { background: #15151a; }
.evt .ts { font-size: 11px; }
.evt .kind { font-size: 11px; }
.evt.evt-decision .body { color: var(--assistant); font-style: italic; }
.evt.evt-tool_call .tool { color: var(--accent); }
.evt.evt-tool_call .args { color: var(--dim); }
.evt.evt-tool_result .body { color: var(--dim); }
.evt.evt-verdict .body { font-weight: 600; }
.ok { color: var(--ok); }
.err { color: var(--err); }
.warn { color: var(--warn); }
.kv { display: flex; justify-content: space-between; padding: 0.15em 0; font-size: 12px; }
.kv span { color: var(--fade); }
.kv b { color: var(--fg); font-weight: 400; }
.s-running { color: var(--warn); }
.s-completed { color: var(--ok); }
.s-failed { color: var(--err); }
.s-cancelled { color: var(--fade); }
.models .model { padding: 0.15em 0; font-size: 12px; }
.dot.ok { color: var(--ok); }
.dot.err { color: var(--err); }
.dot.off { color: var(--fade); }
.badge { background: var(--accent); color: #1a1408; padding: 0.05em 0.5em; font-size: 11px; font-weight: 600; vertical-align: middle; }
"#;
