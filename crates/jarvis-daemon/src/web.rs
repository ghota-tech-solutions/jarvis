//! Embedded web UI served alongside the gRPC API.
//!
//! Borderless, OpenCode-ish aesthetic. HTMX for live updates (poll the JSON
//! endpoints, swap inner HTML). One single template file rendered with `format!`
//! macros — no template engine, no static assets, no JS framework.

use anyhow::Context as _;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use futures::stream::Stream;
use std::convert::Infallible;
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
        .route("/api/events/stream", get(api_events_sse))
        .route("/api/projects", get(api_projects))
        .route("/api/git", get(api_git))
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

// ----------------- SSE event stream -----------------

#[derive(Deserialize)]
struct SseQuery {
    task: String,
    #[serde(default)]
    since: i64,
}

async fn api_events_sse(
    State(s): State<WebState>,
    Query(q): Query<SseQuery>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, AppError> {
    let task_id =
        TaskId::from_str(&q.task).map_err(|_| AppError::BadRequest("invalid task id".into()))?;
    let chain = s.ledger.walk_ancestors(task_id).await.unwrap_or_default();
    let chain_ids: std::collections::HashSet<TaskId> = chain.iter().map(|t| t.id).collect();
    let backfill = s
        .ledger
        .query_events_multi(&chain.iter().map(|t| t.id).collect::<Vec<_>>(), q.since, 0)
        .await
        .unwrap_or_default();
    let mut live = s.ledger.subscribe();

    let stream = async_stream::stream! {
        for ev in backfill {
            let html = render_event_blocks(std::slice::from_ref(&ev));
            yield Ok::<_, Infallible>(
                SseEvent::default()
                    .event("event")
                    .id(ev.id.0.to_string())
                    .data(html)
            );
        }
        loop {
            match live.recv().await {
                Ok(ev) if chain_ids.contains(&ev.task_id) => {
                    let html = render_event_blocks(std::slice::from_ref(&ev));
                    yield Ok(
                        SseEvent::default()
                            .event("event")
                            .id(ev.id.0.to_string())
                            .data(html)
                    );
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
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

#[derive(Deserialize)]
struct GitQuery {
    workdir: String,
}

async fn api_git(Query(q): Query<GitQuery>) -> Response {
    let workdir = q.workdir.clone();
    let info = tokio::task::spawn_blocking(move || git_status(&workdir)).await;
    match info {
        Ok(Some(info)) => Html(render_git_card(&info)).into_response(),
        _ => Html(
            r#"<div class="muted small">not a git repo</div>"#.to_string(),
        )
        .into_response(),
    }
}

fn render_git_card(info: &GitInfo) -> String {
    let mut files_html = String::new();
    for f in info.files.iter().take(16) {
        let cls = match f.status.as_str() {
            "new" => "add",
            "deleted" => "rem",
            _ => "mod",
        };
        files_html.push_str(&format!(
            r#"<div class="git-file"><span class="git-tag {cls}">{tag}</span> <code>{path}</code></div>"#,
            tag = match f.status.as_str() {
                "new" => "A",
                "deleted" => "D",
                "modified" => "M",
                "renamed" => "R",
                _ => "?",
            },
            path = html_escape(&f.path),
        ));
    }
    let more = if info.files.len() > 16 {
        format!(
            r#"<div class="muted small">… and {} more</div>"#,
            info.files.len() - 16
        )
    } else {
        String::new()
    };
    format!(
        r#"<div class="kv"><span>branch</span><b>{branch}</b></div><div class="kv"><span>changes</span><b><span class="add">+{add}</span> <span class="rem">-{rem}</span></b></div>{files_html}{more}"#,
        branch = html_escape(&info.branch),
        add = info.changes_added,
        rem = info.changes_removed,
    )
}

#[derive(Serialize)]
struct GitInfo {
    available: bool,
    branch: String,
    changes_added: usize,
    changes_removed: usize,
    files: Vec<GitFileChange>,
}

#[derive(Serialize)]
struct GitFileChange {
    path: String,
    status: String,    // "modified" | "new" | "deleted" | "renamed"
}

fn git_status(workdir: &str) -> Option<GitInfo> {
    use git2::{Repository, Status, StatusOptions};
    let repo = Repository::discover(workdir).ok()?;
    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(|s| s.to_string()))
        .unwrap_or_else(|| "(detached)".to_string());

    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(false);
    let statuses = repo.statuses(Some(&mut opts)).ok()?;

    let mut files: Vec<GitFileChange> = Vec::new();
    for entry in statuses.iter() {
        let path = entry.path().unwrap_or("").to_string();
        let s = entry.status();
        let status = if s.intersects(Status::WT_NEW | Status::INDEX_NEW) {
            "new"
        } else if s.intersects(Status::WT_DELETED | Status::INDEX_DELETED) {
            "deleted"
        } else if s.intersects(Status::WT_RENAMED | Status::INDEX_RENAMED) {
            "renamed"
        } else if s.intersects(
            Status::WT_MODIFIED | Status::INDEX_MODIFIED | Status::WT_TYPECHANGE | Status::INDEX_TYPECHANGE,
        ) {
            "modified"
        } else {
            continue;
        };
        files.push(GitFileChange {
            path,
            status: status.to_string(),
        });
    }

    // Per-file diff stats via `git diff --numstat` semantics; we use libgit2.
    let mut added_total = 0usize;
    let mut removed_total = 0usize;
    if let Ok(head) = repo.head().and_then(|h| h.peel_to_tree()) {
        let mut diff_opts = git2::DiffOptions::new();
        diff_opts.include_untracked(true).recurse_untracked_dirs(false);
        if let Ok(diff) = repo.diff_tree_to_workdir_with_index(Some(&head), Some(&mut diff_opts)) {
            let stats = diff.stats().ok();
            if let Some(stats) = stats {
                added_total = stats.insertions();
                removed_total = stats.deletions();
            }
        }
    }

    Some(GitInfo {
        available: true,
        branch,
        changes_added: added_total,
        changes_removed: removed_total,
        files,
    })
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
    /// 303 to the given path.
    Redirect(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()).into_response(),
            AppError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
            AppError::Redirect(path) => (
                StatusCode::SEE_OTHER,
                [("Location", path.as_str()), ("HX-Redirect", path.as_str())],
            )
                .into_response(),
        }
    }
}

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
    <section id="new-task" class="card">
      <h2>New task</h2>
      <form hx-post="/api/tasks" hx-encoding="application/x-www-form-urlencoded" class="newtask" hx-on::after-request="if (event.detail.successful) this.reset()">
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
    </section>
    <table class="tasks" hx-get="/api/tasks?all=true&format=html" hx-trigger="every 2s" hx-target="this" hx-swap="outerHTML">
      <thead><tr><th>id</th><th>status</th><th>sandbox</th><th>goal</th><th>created</th></tr></thead>
      <tbody>{task_rows}</tbody>
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
    let mut out = String::new();
    if groups.is_empty() {
        out.push_str(r#"<div class="muted small">no projects yet</div>"#);
        return out;
    }
    for (workdir, tasks) in groups {
        let short = workdir
            .rsplit_once(['/', '\\'])
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| workdir.clone());
        let active = current_workdir
            .map(|w| w == workdir.as_str())
            .unwrap_or(false);
        let open = if active { " open" } else { "" };
        out.push_str(&format!(
            r#"<details class="project{open}"><summary>📁 {short} <span class="muted small">{n}</span></summary>"#,
            open = open,
            short = html_escape(&short),
            n = tasks.len(),
        ));
        // Roots only — display only top-level tasks (parent == null). Children are
        // reachable via continuation. Cap at 12 entries per project.
        for t in tasks.iter().filter(|t| t.parent.is_none()).take(12) {
            let ago = relative_time(t.created_at);
            out.push_str(&format!(
                r#"<a href="/task/{id}" class="project-task t-{status}"><span class="goal">{goal}</span><span class="ago muted small">{ago}</span></a>"#,
                id = t.id,
                status = html_escape(&t.status.to_string()),
                goal = html_escape(&clip(&t.goal, 64)),
                ago = ago,
            ));
        }
        out.push_str("</details>");
    }
    out
}

fn relative_time(micros: i64) -> String {
    let now = chrono::Utc::now().timestamp_micros();
    let dt = (now - micros).max(0);
    let secs = dt / 1_000_000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
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

fn render_task_page(
    root: &TaskRecord,
    leaf: &TaskRecord,
    chain: &[TaskRecord],
    events: &[EventRecord],
    all_tasks: &[TaskRecord],
) -> String {
    let blocks = render_event_blocks(events);
    let chain_label = if chain.len() > 1 {
        format!(r#"<p class="muted small">conversation · {} turns</p>"#, chain.len())
    } else {
        String::new()
    };
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
      <h1>{goal}</h1>
      <p class="muted small">{status} · {backend} · {short_id} {chain_label_inline}</p>
    </header>
    {chain_label}
    <div class="events"
         id="events"
         hx-ext="sse"
         sse-connect="/api/events/stream?task={root_id}&since={last_id}"
         sse-swap="event"
         hx-swap="beforeend">
      {blocks}
    </div>
    <form id="continue" class="continue"
          hx-post="/api/tasks"
          hx-headers='{{"HX-Request": "true"}}'
          hx-swap="none"
          hx-on::after-request="if (event.detail.successful) this.reset()">
      <input type="hidden" name="parent_task_id" value="{leaf_id}">
      <textarea name="goal" rows="2" placeholder="Ask for follow-up changes… (Enter to send)" required></textarea>
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
    <div class="section-label">Task</div>
    <div class="kv"><span>id</span><b>{short_id}</b></div>
    <div class="kv"><span>status</span><b class="s-{status}">{status}</b></div>
    {sandbox_kv}
    <div class="section-label">Workdir</div>
    <div class="muted small monoline">{workdir}</div>
    {worktree_section}
    <div class="section-label">Actions</div>
    <button class="ghost" hx-post="/api/tasks/{leaf_id}/cancel" hx-confirm="Cancel this task?">cancel current</button>
  </aside>
</div>
<script>
// HTMX is configured to ignore SSE 'event' name unless explicitly subscribed.
// Auto-scroll the events container to the bottom on every swap.
document.body.addEventListener('htmx:afterSwap', (e) => {{
  const ev = document.getElementById('events');
  if (ev && e.target && (e.target === ev || ev.contains(e.target))) {{
    ev.scrollTop = ev.scrollHeight;
  }}
}});
// Clear the textarea once submitted, then refocus.
document.body.addEventListener('htmx:afterRequest', (e) => {{
  if (e.target.id === 'continue' && e.detail.successful) {{
    e.target.querySelector('textarea').focus();
  }}
}});
</script>
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
        chain_label = chain_label,
        chain_label_inline = if chain.len() > 1 {
            format!("· {} turns", chain.len())
        } else {
            String::new()
        },
        blocks = blocks,
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
    )
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            _ => {
                for b in c.to_string().as_bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
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
                // For fs_write, expose the diff as a collapsible card.
                let diff_card = ev
                    .payload
                    .get("data")
                    .and_then(render_diff_card);
                if let Some(card) = diff_card {
                    format!("{glyph} {}{}", html_escape(summary), card)
                } else {
                    format!("{glyph} {}", html_escape(summary))
                }
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

/// If `data` looks like an fs_write payload, return a `<details>` diff card.
fn render_diff_card(data: &serde_json::Value) -> Option<String> {
    let path = data.get("path")?.as_str()?;
    let added = data.get("lines_added")?.as_u64()?;
    let removed = data.get("lines_removed")?.as_u64()?;
    let is_new = data.get("is_new").and_then(|v| v.as_bool()).unwrap_or(false);
    let diff = data
        .get("diff_unified")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let badge = if is_new { "new" } else { "edit" };
    let body = colorize_unified(diff);
    Some(format!(
        r#"<details class="diff"><summary><span class="badge {badge_cls}">{badge}</span> <code>{path}</code> <span class="add">+{added}</span> <span class="rem">-{removed}</span></summary><pre class="diff-body">{body}</pre></details>"#,
        badge_cls = badge,
        path = html_escape(path),
        body = body,
    ))
}

fn colorize_unified(diff: &str) -> String {
    let mut out = String::with_capacity(diff.len());
    for line in diff.lines() {
        let (class, _) = if line.starts_with('+') {
            ("add", "+")
        } else if line.starts_with('-') {
            ("rem", "-")
        } else {
            ("ctx", " ")
        };
        out.push_str(&format!(
            r#"<span class="d-{class}">{}</span>{}"#,
            html_escape(line),
            "\n"
        ));
    }
    out
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
  --bg: #0a0a0c;
  --panel: #111114;
  --panel-2: #15151a;
  --line: #1f1f25;
  --fg: #c8c8c8;
  --dim: #808080;
  --fade: #505050;
  --heading: #e6dcc8;
  --accent: #d4b478;
  --ok: #8cb46e;
  --err: #dc6e5a;
  --warn: #dcaf5a;
  --assistant: #b4c8dc;
  --link: #d4b478;
  --add-bg: rgba(140,180,110,0.10);
  --rem-bg: rgba(220,110,90,0.10);
  --add-fg: #b6dc94;
  --rem-fg: #f0a292;
}
* { box-sizing: border-box; }
html, body { background: var(--bg); color: var(--fg); margin: 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; line-height: 1.55; height: 100%; }
a { color: var(--link); text-decoration: none; }
a:hover { text-decoration: underline; }
code { background: var(--panel-2); padding: 0.05em 0.3em; border-radius: 3px; font-size: 0.95em; color: var(--fg); }
h1 { font-size: 17px; font-weight: 600; color: var(--heading); margin: 0 0 0.4em; }
h2 { font-size: 13px; color: var(--heading); margin: 1.4em 0 0.5em; font-weight: 600; }
p { margin: 0.3em 0; }
.muted { color: var(--dim); }
.small { font-size: 12px; }
.monoline { word-break: break-all; overflow-wrap: anywhere; }

/* 3-column shell */
.shell { display: grid; grid-template-columns: 260px minmax(0, 1fr) 320px; min-height: 100vh; }
.left { background: var(--panel); border-right: 1px solid var(--line); padding: 1em 0.8em; overflow-y: auto; }
.main { padding: 1.4em 2em; min-width: 0; }
.right { background: var(--panel); border-left: 1px solid var(--line); padding: 1em 0.8em; overflow-y: auto; }

/* Left nav */
.brand { font-weight: 700; color: var(--heading); padding: 0.2em 0.4em 1em; font-size: 14px; }
.navitem { display: block; padding: 0.35em 0.5em; color: var(--fg); border-radius: 4px; }
.navitem:hover { background: var(--panel-2); text-decoration: none; }
.navitem.active { background: var(--panel-2); color: var(--heading); }
.section-label { color: var(--fade); font-size: 11px; text-transform: uppercase; letter-spacing: 0.08em; margin: 1.2em 0.5em 0.4em; font-weight: 600; }
details.project > summary { padding: 0.3em 0.5em; cursor: pointer; border-radius: 4px; list-style: none; color: var(--fg); }
details.project > summary::-webkit-details-marker { display: none; }
details.project > summary:hover { background: var(--panel-2); }
details.project[open] > summary { color: var(--heading); }
.project-task { display: flex; justify-content: space-between; align-items: baseline; padding: 0.2em 0.5em 0.2em 1.4em; color: var(--dim); border-radius: 4px; gap: 0.5em; }
.project-task:hover { background: var(--panel-2); color: var(--fg); text-decoration: none; }
.project-task .goal { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.project-task .ago { flex-shrink: 0; }
.project-task.t-running .goal { color: var(--warn); }
.project-task.t-completed .goal { color: var(--ok); }
.project-task.t-failed .goal { color: var(--err); }
.navfoot { padding: 1em 0.4em; border-top: 1px solid var(--line); margin-top: 1em; }

/* Main */
.task-header { margin-bottom: 1em; }
.card { background: var(--panel); border: 1px solid var(--line); border-radius: 6px; padding: 1em; margin: 0.6em 0; }
table.tasks { width: 100%; border-collapse: collapse; font-size: 13px; margin-top: 1em; }
table.tasks th { text-align: left; color: var(--fade); font-weight: 400; padding: 0.4em 0.5em; border-bottom: 1px solid var(--line); }
table.tasks td { padding: 0.4em 0.5em; vertical-align: top; }
table.tasks tr:hover td { background: var(--panel-2); }
tr.t-completed td:nth-child(2) { color: var(--ok); }
tr.t-running td:nth-child(2) { color: var(--warn); }
tr.t-failed td:nth-child(2) { color: var(--err); }
tr.t-cancelled td:nth-child(2) { color: var(--fade); }

/* Forms */
form.newtask textarea, form.continue textarea { width: 100%; background: var(--panel-2); color: var(--fg); border: 1px solid var(--line); padding: 0.6em 0.7em; font-family: inherit; font-size: 13px; resize: vertical; border-radius: 4px; }
form.newtask textarea:focus, form.continue textarea:focus { outline: none; border-color: var(--accent); }
form .row { display: flex; gap: 0.5em; margin-top: 0.5em; flex-wrap: wrap; align-items: center; }
form input[type=text], form input:not([type]), form select { background: var(--panel-2); color: var(--fg); border: 1px solid var(--line); padding: 0.35em 0.5em; font-family: inherit; font-size: 12px; border-radius: 3px; }
form button { background: var(--accent); color: #1a1408; border: none; padding: 0.45em 1em; font-family: inherit; font-size: 12px; font-weight: 600; cursor: pointer; border-radius: 4px; }
form button:hover { background: #e0c890; }
form button.ghost { background: transparent; color: var(--fg); border: 1px solid var(--line); }
form button.ghost:hover { background: var(--panel-2); color: var(--err); }
.checkbox { display: flex; gap: 0.3em; align-items: center; color: var(--dim); font-size: 12px; }

/* Continue (follow-up) */
form.continue { position: sticky; bottom: 0; background: var(--bg); padding: 0.6em 0; border-top: 1px solid var(--line); margin-top: 1em; }
.continue-actions { display: flex; justify-content: flex-end; margin-top: 0.4em; }

/* Events stream */
.events { display: flex; flex-direction: column; gap: 0.15em; max-height: calc(100vh - 250px); overflow-y: auto; padding-right: 0.5em; }
.evt { display: grid; grid-template-columns: 70px 110px 1fr; gap: 0.5em; padding: 0.2em 0.3em; align-items: baseline; border-radius: 4px; }
.evt:hover { background: var(--panel-2); }
.evt .ts { font-size: 11px; }
.evt .kind { font-size: 11px; }
.evt.evt-decision .body { color: var(--assistant); font-style: italic; }
.evt.evt-tool_call .tool { color: var(--accent); }
.evt.evt-tool_call .args { color: var(--dim); }
.evt.evt-tool_result .body { color: var(--dim); }
.evt.evt-verdict .body { font-weight: 600; }

/* Diff card */
details.diff { display: inline-block; margin-left: 0.6em; vertical-align: baseline; }
details.diff > summary { cursor: pointer; padding: 0.1em 0.5em; background: var(--panel-2); border-radius: 4px; border: 1px solid var(--line); list-style: none; font-size: 12px; }
details.diff > summary::-webkit-details-marker { display: none; }
details.diff > summary:hover { border-color: var(--accent); }
details.diff .badge { padding: 0.05em 0.4em; font-size: 10px; font-weight: 600; border-radius: 3px; margin-right: 0.3em; text-transform: uppercase; }
details.diff .badge.new { background: var(--add-bg); color: var(--add-fg); }
details.diff .badge.edit { background: rgba(212,180,120,0.15); color: var(--accent); }
details.diff[open] > summary { border-bottom-left-radius: 0; border-bottom-right-radius: 0; border-bottom-color: transparent; }
pre.diff-body { background: var(--panel-2); border: 1px solid var(--line); border-top: none; padding: 0.6em 0.8em; font-size: 12px; line-height: 1.45; overflow-x: auto; margin: 0; border-radius: 0 0 4px 4px; max-width: 100%; }
.d-add { background: var(--add-bg); color: var(--add-fg); display: block; }
.d-rem { background: var(--rem-bg); color: var(--rem-fg); display: block; }
.d-ctx { color: var(--dim); display: block; }
.add { color: var(--add-fg); }
.rem { color: var(--rem-fg); }
.ok { color: var(--ok); }
.err { color: var(--err); }
.warn { color: var(--warn); }

/* Sidebar (right) */
.kv { display: flex; justify-content: space-between; padding: 0.2em 0.4em; font-size: 12px; }
.kv span { color: var(--fade); }
.kv b { color: var(--fg); font-weight: 400; }
.s-running { color: var(--warn); }
.s-completed { color: var(--ok); }
.s-failed { color: var(--err); }
.s-cancelled { color: var(--fade); }
.models .model { padding: 0.2em 0.4em; font-size: 12px; }
.dot.ok { color: var(--ok); }
.dot.err { color: var(--err); }
.dot.off { color: var(--fade); }

.git-card { background: var(--panel-2); border: 1px solid var(--line); border-radius: 4px; padding: 0.6em; margin-bottom: 0.4em; }
.git-file { padding: 0.15em 0.2em; font-size: 12px; display: flex; gap: 0.4em; align-items: baseline; }
.git-tag { display: inline-block; width: 16px; text-align: center; font-size: 10px; font-weight: 600; padding: 0.05em 0.2em; border-radius: 2px; }
.git-tag.add { background: var(--add-bg); color: var(--add-fg); }
.git-tag.rem { background: var(--rem-bg); color: var(--rem-fg); }
.git-tag.mod { background: rgba(220,175,90,0.15); color: var(--warn); }

@media (max-width: 1200px) {
  .shell { grid-template-columns: 220px 1fr; }
  .right { display: none; }
}
@media (max-width: 800px) {
  .shell { grid-template-columns: 1fr; }
  .left { display: none; }
}
"#;
