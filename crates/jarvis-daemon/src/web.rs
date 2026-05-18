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
    let subscribed_root =
        TaskId::from_str(&q.task).map_err(|_| AppError::BadRequest("invalid task id".into()))?;
    let chain = s.ledger.walk_ancestors(subscribed_root).await.unwrap_or_default();
    let backfill = s
        .ledger
        .query_events_multi(&chain.iter().map(|t| t.id).collect::<Vec<_>>(), q.since, 0)
        .await
        .unwrap_or_default();
    let mut live = s.ledger.subscribe();

    // Per-event cache of "this task's chain root" so we don't re-walk on every
    // event for the same task. Children dynamically attach to the chain — that's
    // why we walk ancestors instead of capturing chain_ids statically.
    let mut root_cache: std::collections::HashMap<TaskId, TaskId> =
        std::collections::HashMap::new();
    for t in &chain {
        root_cache.insert(t.id, subscribed_root);
    }
    let ledger = s.ledger.clone();

    let stream = async_stream::stream! {
        for ev in backfill {
            if let Some(sse) = render_event_for_sse(&ev) {
                yield Ok::<_, Infallible>(sse);
            }
        }
        loop {
            match live.recv().await {
                Ok(ev) => {
                    if descends_from(&ledger, &mut root_cache, ev.task_id, subscribed_root).await
                        && let Some(sse) = render_event_for_sse(&ev)
                    {
                        yield Ok(sse);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Decide how to surface a ledger event on the SSE stream. Returns None when
/// the event should be silenced (heartbeat, attempt, llm_chunk that we'd rather
/// emit as a chunk-type event handled in JS).
fn render_event_for_sse(ev: &EventRecord) -> Option<SseEvent> {
    let kind = ev.kind.to_string();
    match kind.as_str() {
        // Live token deltas — sent as a dedicated event type the page listens to.
        "llm_chunk" => {
            let delta = ev.payload.get("delta").and_then(|v| v.as_str())?;
            // The front-end handles plain text; encode minimally for SSE (no HTML escape
            // here because we'll insert as textContent on the JS side).
            Some(
                SseEvent::default()
                    .event("chunk")
                    .id(ev.id.0.to_string())
                    .data(delta),
            )
        }
        // Decision arriving means the streamed text is now complete — signal
        // the front-end to clear the live composing buffer, then send the full
        // rendered block.
        "decision" => {
            let html = render_event_blocks(std::slice::from_ref(ev));
            Some(
                SseEvent::default()
                    .event("decision")
                    .id(ev.id.0.to_string())
                    .data(html),
            )
        }
        // Noise we don't want on the page at all.
        // tool_call is silenced because the matching tool_result carries `args`
        // (since M6.7) and renders the full action card on its own.
        "heartbeat" | "attempt" | "tool_call" => None,
        _ => {
            let html = render_event_blocks(std::slice::from_ref(ev));
            Some(
                SseEvent::default()
                    .event("event")
                    .id(ev.id.0.to_string())
                    .data(html),
            )
        }
    }
}

/// Returns true if `task_id` is `root` or any ancestor of `task_id` is `root`.
/// Caches results keyed by task_id so we don't re-walk for streaming events
/// that come from the same task in bursts.
async fn descends_from(
    ledger: &Ledger,
    cache: &mut std::collections::HashMap<TaskId, TaskId>,
    task_id: TaskId,
    root: TaskId,
) -> bool {
    if let Some(known_root) = cache.get(&task_id) {
        return *known_root == root;
    }
    let mut current = Some(task_id);
    let mut walked = Vec::new();
    while let Some(id) = current {
        walked.push(id);
        if id == root {
            for w in &walked {
                cache.insert(*w, root);
            }
            return true;
        }
        if let Some(known_root) = cache.get(&id).copied() {
            let same = known_root == root;
            for w in &walked {
                cache.insert(*w, known_root);
            }
            return same;
        }
        match ledger.get_task(id).await {
            Ok(t) => current = t.parent,
            Err(_) => return false,
        }
    }
    false
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
        <textarea name="goal" rows="2" placeholder="Describe the goal — Enter to submit, Shift+Enter for a newline" required></textarea>
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
    <script>
      // Enter to submit on the dashboard form too.
      document.querySelectorAll('textarea[name="goal"]').forEach((ta) => {{
        ta.addEventListener('keydown', (e) => {{
          if (e.key === 'Enter' && !e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {{
            e.preventDefault();
            if (typeof ta.form.requestSubmit === 'function') ta.form.requestSubmit();
            else ta.form.submit();
          }}
        }});
      }});
    </script>
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
        let roots: Vec<_> = tasks.iter().filter(|t| t.parent.is_none()).collect();
        // Each root is one conversation. Children show up rendered inside the
        // conversation via the ancestor chain.
        out.push_str(&format!(
            r#"<details class="project{open}"><summary>📁 {short} <span class="muted small">{n}</span></summary>"#,
            open = open,
            short = html_escape(&short),
            n = roots.len(),
        ));
        for t in roots.iter().take(12) {
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

/// One row per conversation: the root's goal heads the row, the leaf's status
/// is the displayed status, and a turn count shows how many follow-ups live in
/// the chain. Mirrors the way the projects nav and detail page see the data.
fn render_task_rows(tasks: &[TaskRecord]) -> String {
    let conversations = group_into_conversations(tasks);
    let mut s = String::new();
    if conversations.is_empty() {
        s.push_str(r#"<tr><td colspan="6" class="muted">no tasks yet</td></tr>"#);
        return s;
    }
    for c in conversations {
        let backend = if c.leaf_sandbox.is_empty() {
            String::from("-")
        } else if c.leaf_sandbox == "native" {
            "native".to_string()
        } else {
            format!("{}/{}", c.leaf_sandbox, c.leaf_net_policy)
        };
        let turns = if c.turn_count > 1 {
            format!(r#"<span class="turns-badge">{}</span>"#, c.turn_count)
        } else {
            r#"<span class="muted small">1</span>"#.to_string()
        };
        s.push_str(&format!(
            r#"<tr class="t-{status}"><td><a href="/task/{id}">{short}</a></td><td>{status}</td><td>{backend}</td><td>{goal}</td><td>{turns}</td><td class="muted">{updated}</td></tr>"#,
            id = c.root_id,
            short = short(&c.root_id.to_string()),
            status = html_escape(&c.leaf_status),
            backend = html_escape(&backend),
            goal = html_escape(&clip(&c.root_goal, 120)),
            updated = relative_time(c.updated_at),
        ));
    }
    s
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
    rows.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
    rows
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
// === Smart-follow auto-scroll =================================================
// The page (window) scrolls now — not the events container — so we follow on
// window.scrollY. We track "following" state: when the user has scrolled near
// the bottom we keep snapping; if they scroll up we pause and show a floating
// "↓ N new" button to jump back.
(function setupSmartFollow() {{
  const SLACK = 80;
  const jumpBtn = document.getElementById('jump-bottom');
  const jumpCount = document.getElementById('jump-count');
  const isAtBottom = () => (window.innerHeight + window.scrollY) >= (document.documentElement.scrollHeight - SLACK);
  let following = true;
  let unseen = 0;
  function refreshJump() {{
    if (!jumpBtn) return;
    if (!following && unseen > 0) {{
      jumpCount.textContent = unseen + ' new';
      jumpBtn.classList.add('show');
    }} else {{
      jumpBtn.classList.remove('show');
    }}
  }}
  function snapBottom() {{ window.scrollTo({{top: document.documentElement.scrollHeight, behavior: 'instant'}}); }}
  window.addEventListener('scroll', () => {{
    following = isAtBottom();
    if (following) {{ unseen = 0; refreshJump(); }}
  }}, {{passive: true}});
  jumpBtn?.addEventListener('click', () => {{
    following = true;
    unseen = 0;
    snapBottom();
    refreshJump();
  }});
  window.__jarvisFollow = {{
    onNew() {{ if (following) snapBottom(); else {{ unseen++; refreshJump(); }} }},
    isFollowing() {{ return following; }},
  }};
  // Initial scroll-to-bottom on load.
  window.addEventListener('load', () => snapBottom());
}})();

// === Streaming composer + thinking timer ======================================
(function setupStreamingComposer() {{
  const composing = document.getElementById('composing');
  if (!composing) return;
  const events = document.getElementById('events');
  if (!events) return;

  let thinkingEl = null;
  let thinkingTimer = null;
  const THINKING_DELAY = 1500;

  function showThinking() {{
    if (thinkingEl || !composing.hidden) return;
    thinkingEl = document.createElement('div');
    thinkingEl.className = 'turn thinking';
    thinkingEl.innerHTML = '<span class="dots"><span></span><span></span><span></span></span><span>thinking…</span>';
    composing.parentNode.insertBefore(thinkingEl, composing);
    window.__jarvisFollow?.onNew();
  }}
  function clearThinking() {{
    if (thinkingTimer) {{ clearTimeout(thinkingTimer); thinkingTimer = null; }}
    if (thinkingEl) {{ thinkingEl.remove(); thinkingEl = null; }}
  }}
  function armThinking() {{
    clearThinking();
    thinkingTimer = setTimeout(showThinking, THINKING_DELAY);
  }}

  function fadeOutComposing() {{
    composing.classList.add('fade-out');
    setTimeout(() => {{
      composing.hidden = true;
      composing.textContent = '';
      composing.classList.remove('fade-out');
    }}, 220);
  }}

  function tryAttach() {{
    const es = events.__sse?.source || htmx.find(events)?.__sse?.source;
    if (!es || es._jarvis_attached) {{
      setTimeout(tryAttach, 100);
      return;
    }}
    es._jarvis_attached = true;
    es.addEventListener('open', () => armThinking());
    es.addEventListener('chunk', (ev) => {{
      clearThinking();
      if (composing.hidden) {{ composing.hidden = false; composing.textContent = ''; composing.classList.remove('fade-out'); }}
      composing.textContent += ev.data;
      window.__jarvisFollow?.onNew();
    }});
    es.addEventListener('decision', (ev) => {{
      clearThinking();
      fadeOutComposing();
      // Insert the rendered decision block AFTER the composing element.
      events.insertAdjacentHTML('beforeend', ev.data);
      window.__jarvisFollow?.onNew();
      armThinking();
    }});
    es.addEventListener('event', () => {{
      // Any non-chunk event resets the thinking timer.
      clearThinking();
      armThinking();
      window.__jarvisFollow?.onNew();
    }});
  }}
  tryAttach();
}})();

// === Auto-scroll on HTMX swaps (covers initial backfill + sse swaps) ==========
document.body.addEventListener('htmx:afterSwap', () => {{
  window.__jarvisFollow?.onNew();
}});

// === Continue form: refocus + clear textarea after submit =====================
document.body.addEventListener('htmx:afterRequest', (e) => {{
  if (e.target.id === 'continue' && e.detail.successful) {{
    const ta = e.target.querySelector('textarea');
    if (ta) {{ ta.value = ''; ta.style.height = 'auto'; ta.focus(); }}
  }}
}});

// === Auto-resize textareas as you type ========================================
function autoResize(ta) {{
  ta.style.height = 'auto';
  const next = Math.min(ta.scrollHeight, 300);
  ta.style.height = next + 'px';
}}
document.querySelectorAll('textarea[name="goal"]').forEach((ta) => {{
  ta.addEventListener('input', () => autoResize(ta));
  ta.addEventListener('keydown', (e) => {{
    if (e.key === 'Enter' && !e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {{
      e.preventDefault();
      if (typeof ta.form.requestSubmit === 'function') ta.form.requestSubmit();
      else ta.form.submit();
    }}
  }});
}});

// === "+ Continue" link in the left nav: focus + scroll the textarea ===========
document.querySelectorAll('a[href="#continue"]').forEach((a) => {{
  a.addEventListener('click', (e) => {{
    e.preventDefault();
    const ta = document.querySelector('#continue textarea');
    if (ta) {{ ta.focus(); ta.scrollIntoView({{behavior: 'smooth', block: 'end'}}); }}
  }});
}});

// === Auto-focus the continue textarea on page load ============================
window.addEventListener('load', () => {{
  document.querySelector('#continue textarea')?.focus();
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

/// Render a slice of events as Codex-style blocks. Hides infrastructure noise
/// (heartbeat, attempt). Pairs tool_call+tool_result into single action cards.
/// Decision/verdict become prose blocks. When `pending_tool_call` carries state
/// across SSE yields, callers can pass it in; here we fold per-call.
fn render_event_blocks(events: &[EventRecord]) -> String {
    let mut out = String::new();
    let mut pending_tool: Option<&EventRecord> = None;
    for ev in events {
        match ev.kind.to_string().as_str() {
            // Noise: skip entirely. Step boundaries are implied by tool runs.
            "heartbeat" | "attempt" => {}
            "decision" => {
                pending_tool = None;
                if let Some(t) = ev.payload.get("thought").and_then(|v| v.as_str())
                    && !t.trim().is_empty()
                {
                    out.push_str(&format!(
                        r#"<div class="turn assistant" data-id="{id}">{}</div>"#,
                        render_markdown(t),
                        id = ev.id.0,
                    ));
                }
            }
            "tool_call" => {
                pending_tool = Some(ev);
            }
            "tool_result" => {
                let pair_id = pending_tool.map(|c| c.id.0).unwrap_or(ev.id.0);
                out.push_str(&render_action_card(pending_tool, ev, pair_id));
                pending_tool = None;
            }
            "error" => {
                pending_tool = None;
                let msg = ev
                    .payload
                    .get("message")
                    .or_else(|| ev.payload.get("error"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no message)");
                out.push_str(&format!(
                    r#"<div class="turn error" data-id="{id}"><span class="err">✗ {}</span></div>"#,
                    html_escape(msg),
                    id = ev.id.0,
                ));
            }
            "verdict" => {
                pending_tool = None;
                let v = ev.payload.get("verdict").and_then(|x| x.as_str()).unwrap_or("?");
                let m = ev.payload.get("message").and_then(|x| x.as_str()).unwrap_or("");
                let cls = match v {
                    "pass" => "verdict-pass",
                    "fail" => "verdict-fail",
                    _ => "verdict-other",
                };
                out.push_str(&format!(
                    r#"<div class="turn verdict {cls}" data-id="{id}"><span class="badge {cls}">{v}</span>{}</div>"#,
                    render_markdown(m),
                    id = ev.id.0,
                    cls = cls,
                ));
            }
            _ => {}
        }
    }
    // If a tool_call had no matching tool_result yet (e.g. streaming), surface it alone.
    if let Some(ev) = pending_tool {
        out.push_str(&render_action_card(Some(ev), ev, ev.id.0));
    }
    out
}

/// Compact action card combining a tool_call with its tool_result.
/// Group events by task_id and render each task as its own "turn" block:
/// optional `[user] goal` header (skipped for the root), then the rendered
/// events of that task. The root's goal already lives in the page header.
fn render_conversation(chain: &[TaskRecord], events: &[EventRecord]) -> String {
    use std::collections::HashMap;
    let mut by_task: HashMap<TaskId, Vec<&EventRecord>> = HashMap::new();
    for ev in events {
        by_task.entry(ev.task_id).or_default().push(ev);
    }
    let mut out = String::new();
    // Chain is root → leaf, so render in chain order to keep chronology.
    for (idx, task) in chain.iter().enumerate() {
        // The root's goal is the page H1; only children get a user-message bubble.
        if idx > 0 {
            out.push_str(&format!(
                r#"<div class="turn user"><div class="user-bubble">{}</div></div>"#,
                render_markdown(&task.goal)
            ));
        }
        if let Some(evs) = by_task.get(&task.id) {
            out.push_str(&render_event_blocks_owned(evs));
        }
    }
    out
}

fn render_event_blocks_owned(events: &[&EventRecord]) -> String {
    // Reuse render_event_blocks by reborrowing.
    let owned: Vec<EventRecord> = events.iter().map(|e| (*e).clone()).collect();
    render_event_blocks(&owned)
}

/// Walk the events, aggregate fs_write into a "Edited N files" rollup card.
fn render_files_rollup(events: &[EventRecord]) -> String {
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Agg {
        added: u64,
        removed: u64,
        is_new: bool,
        turns: u32,
    }
    let mut by_path: BTreeMap<String, Agg> = BTreeMap::new();
    for ev in events {
        if ev.kind.to_string() != "tool_result" {
            continue;
        }
        let Some(data) = ev.payload.get("data") else { continue };
        let Some(path) = data.get("path").and_then(|v| v.as_str()) else { continue };
        let Some(added) = data.get("lines_added").and_then(|v| v.as_u64()) else { continue };
        let removed = data.get("lines_removed").and_then(|v| v.as_u64()).unwrap_or(0);
        let is_new = data.get("is_new").and_then(|v| v.as_bool()).unwrap_or(false);
        let entry = by_path.entry(path.to_string()).or_default();
        entry.added += added;
        entry.removed += removed;
        entry.is_new = entry.is_new || is_new;
        entry.turns += 1;
    }
    if by_path.is_empty() {
        return String::new();
    }
    let total_added: u64 = by_path.values().map(|a| a.added).sum();
    let total_removed: u64 = by_path.values().map(|a| a.removed).sum();
    let file_count = by_path.len();
    let label = if file_count == 1 { "file" } else { "files" };
    let mut rows = String::new();
    for (path, a) in &by_path {
        let short_path = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let badge = if a.is_new && a.turns == 1 {
            r#"<span class="badge new">new</span>"#
        } else {
            r#"<span class="badge edit">edit</span>"#
        };
        rows.push_str(&format!(
            r#"<div class="file-row">{badge}<span class="file-name" title="{full}">{short}</span><span class="add">+{add}</span><span class="rem">-{rem}</span></div>"#,
            full = html_escape(path),
            short = html_escape(&short_path),
            add = a.added,
            rem = a.removed,
        ));
    }
    format!(
        r#"<div class="rollup card"><div class="rollup-head"><span class="rollup-title">Edited {file_count} {label}</span><span class="add">+{total_added}</span><span class="rem">-{total_removed}</span></div><div class="rollup-body">{rows}</div></div>"#,
    )
}

fn render_action_card(call: Option<&EventRecord>, result: &EventRecord, anchor: i64) -> String {
    let tool = call
        .and_then(|c| c.payload.get("tool"))
        .or_else(|| result.payload.get("tool"))
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let args = call.and_then(|c| c.payload.get("args")).cloned().unwrap_or(serde_json::Value::Null);
    let summary = result
        .payload
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let is_error = result
        .payload
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let data = result.payload.get("data");

    let (inner, data_tool, data_new) = match tool.as_str() {
        "shell" => {
            let cmd = args.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
            let exit = data
                .and_then(|d| d.get("exit_code"))
                .map(|x| x.to_string())
                .unwrap_or_default();
            let stdout = data
                .and_then(|d| d.get("stdout"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let stderr = data
                .and_then(|d| d.get("stderr"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let backend = data
                .and_then(|d| d.get("backend"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let exit_chip = if !exit.is_empty() {
                format!(r#"<span class="action-chip">exit {exit}</span>"#)
            } else {
                String::new()
            };
            let backend_chip = if !backend.is_empty() {
                format!(r#"<span class="action-chip muted-chip">{backend}</span>"#)
            } else {
                String::new()
            };
            let output_section = render_inline_output(stdout, stderr);
            (
                format!(
                    r#"<div class="action-head"><span class="action-tool">$</span> <code class="action-cmd">{cmd}</code> <span class="chip-spacer"></span>{exit_chip}{backend_chip}</div>{output_section}"#,
                    cmd = html_escape(cmd),
                ),
                "shell",
                false,
            )
        }
        "fs_read" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let bytes = data
                .and_then(|d| d.get("bytes"))
                .map(|x| x.to_string())
                .unwrap_or_default();
            let bytes_chip = if !bytes.is_empty() {
                format!(r#"<span class="action-chip">{bytes} B</span>"#)
            } else {
                String::new()
            };
            let preview = data
                .and_then(|d| d.get("content"))
                .and_then(|v| v.as_str())
                .map(|c| render_inline_output(c, ""))
                .unwrap_or_default();
            (
                format!(
                    r#"<div class="action-head"><span class="action-verb">Read</span> <code class="action-path">{path}</code> <span class="chip-spacer"></span>{bytes_chip}</div>{preview}"#,
                    path = html_escape(&path),
                ),
                "fs_read",
                false,
            )
        }
        "fs_write" => {
            let path = data
                .and_then(|d| d.get("path"))
                .and_then(|v| v.as_str())
                .or_else(|| args.get("path").and_then(|v| v.as_str()))
                .unwrap_or("?")
                .to_string();
            let added = data
                .and_then(|d| d.get("lines_added"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let removed = data
                .and_then(|d| d.get("lines_removed"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let is_new = data
                .and_then(|d| d.get("is_new"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let verb = if is_new { "Created" } else { "Edited" };
            let unified = data
                .and_then(|d| d.get("diff_unified"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let diff_block = render_inline_diff(unified);
            (
                format!(
                    r#"<div class="action-head"><span class="action-verb">{verb}</span> <code class="action-path">{path}</code> <span class="chip-spacer"></span><span class="add">+{added}</span> <span class="rem">-{removed}</span></div>{diff_block}"#,
                    path = html_escape(&path),
                ),
                "fs_write",
                is_new,
            )
        }
        _ => {
            let summary_html = html_escape(summary);
            (
                format!(
                    r#"<div class="action-head"><span class="action-verb">{tool}</span> {summary_html}</div>"#,
                    tool = html_escape(&tool),
                ),
                "other",
                false,
            )
        }
    };

    let state_cls = if is_error { " action-error" } else { "" };
    let new_attr = if data_new { r#" data-new="true""# } else { "" };
    format!(
        r#"<div class="turn action{state_cls}" data-id="{anchor}" data-tool="{data_tool}"{new_attr}>{inner}</div>"#,
    )
}

/// Render shell stdout/stderr inline. Up to 6 visible lines; the rest in a
/// `<details>` "show N more". Nothing if both are empty.
fn render_inline_output(stdout: &str, stderr: &str) -> String {
    fn block(text: &str, extra_cls: &str, label: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        const VISIBLE: usize = 6;
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= VISIBLE {
            return format!(
                r#"<pre class="action-out{cls}">{}</pre>"#,
                html_escape(text),
                cls = if extra_cls.is_empty() { String::new() } else { format!(" {extra_cls}") },
            );
        }
        let head = lines[..VISIBLE].join("\n");
        let tail = lines[VISIBLE..].join("\n");
        let more = lines.len() - VISIBLE;
        let _ = label;
        format!(
            r#"<pre class="action-out{cls}">{head_html}</pre><details class="action-more"><summary class="muted small">show {more} more line{plural}</summary><pre class="action-out{cls}">{tail_html}</pre></details>"#,
            head_html = html_escape(&head),
            tail_html = html_escape(&tail),
            plural = if more > 1 { "s" } else { "" },
            cls = if extra_cls.is_empty() { String::new() } else { format!(" {extra_cls}") },
        )
    }
    let mut s = String::new();
    s.push_str(&block(stdout, "", "stdout"));
    s.push_str(&block(stderr, "action-err", "stderr"));
    s
}

/// Inline diff: ≤20 lines fully visible, else preview 12 lines + show-more.
fn render_inline_diff(unified: &str) -> String {
    if unified.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = unified.lines().collect();
    let html_body = |slice: &[&str]| -> String {
        let chunk = slice.join("\n");
        colorize_unified(&chunk)
    };
    if lines.len() <= 20 {
        return format!(
            r#"<pre class="diff-body">{}</pre>"#,
            html_body(&lines),
        );
    }
    let head = &lines[..12];
    let tail = &lines[12..];
    let more = tail.len();
    format!(
        r#"<pre class="diff-body">{}</pre><details class="action-more"><summary class="muted small">show {more} more line{plural} of diff</summary><pre class="diff-body">{}</pre></details>"#,
        html_body(head),
        html_body(tail),
        plural = if more > 1 { "s" } else { "" },
    )
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
/// Render markdown into safe HTML. We use pulldown-cmark with strict options:
/// raw HTML in the source is escaped, only structural markdown is interpreted.
fn render_markdown(src: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(src, opts);
    // Drop any raw HTML / inline HTML events so untrusted content can't break out.
    let safe = parser.filter(|event| {
        !matches!(
            event,
            pulldown_cmark::Event::Html(_) | pulldown_cmark::Event::InlineHtml(_)
        )
    });
    let mut out = String::with_capacity(src.len() + 32);
    html::push_html(&mut out, safe);
    // Wrap in a span so the CSS can target `.md *` cleanly.
    format!(r#"<span class="md">{out}</span>"#)
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
.left { background: var(--panel); border-right: 1px solid var(--line); padding: 1em 0.8em; overflow-y: auto; position: sticky; top: 0; height: 100vh; }
.main { padding: 1.4em 2em 0; min-width: 0; }
.right { background: var(--panel); border-left: 1px solid var(--line); padding: 1em 0.8em; overflow-y: auto; position: sticky; top: 0; height: 100vh; }

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
.task-header .status-pill {
  display: inline-flex;
  align-items: center;
  gap: 0.4em;
  padding: 0.15em 0.7em;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  vertical-align: middle;
  margin-left: 0.6em;
}
.task-header .status-pill.s-running { background: rgba(220,175,90,0.15); color: var(--warn); }
.task-header .status-pill.s-completed { background: var(--add-bg); color: var(--add-fg); }
.task-header .status-pill.s-failed { background: var(--rem-bg); color: var(--rem-fg); }
.task-header .status-pill.s-cancelled { background: var(--panel-2); color: var(--fade); }
.task-header .status-pill .dot { width: 6px; height: 6px; border-radius: 50%; background: currentColor; }
.task-header .status-pill.s-running .dot { animation: pulse 1.5s ease-in-out infinite; }
@keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.35; } }

/* Floating "jump to bottom" button when smart-follow is paused */
.jump-bottom {
  position: fixed;
  right: 360px;
  bottom: 120px;
  background: var(--accent);
  color: #1a1408;
  border: none;
  border-radius: 999px;
  padding: 0.5em 0.95em;
  font-family: inherit;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  box-shadow: 0 4px 14px rgba(0,0,0,0.4);
  display: none;
  z-index: 20;
}
.jump-bottom.show { display: inline-flex; align-items: center; gap: 0.4em; }
.jump-bottom:hover { background: #e0c890; }
@media (max-width: 1200px) {
  .jump-bottom { right: 24px; }
}
.card { background: var(--panel); border: 1px solid var(--line); border-radius: 6px; padding: 1em; margin: 0.6em 0; }
table.tasks { width: 100%; border-collapse: collapse; font-size: 13px; margin-top: 1em; }
table.tasks th { text-align: left; color: var(--fade); font-weight: 400; padding: 0.4em 0.5em; border-bottom: 1px solid var(--line); }
table.tasks td { padding: 0.4em 0.5em; vertical-align: top; }
table.tasks tr:hover td { background: var(--panel-2); }
tr.t-completed td:nth-child(2) { color: var(--ok); }
tr.t-running td:nth-child(2) { color: var(--warn); }
tr.t-failed td:nth-child(2) { color: var(--err); }
tr.t-cancelled td:nth-child(2) { color: var(--fade); }
.turns-badge { display: inline-block; min-width: 18px; text-align: center; background: rgba(212,180,120,0.15); color: var(--accent); padding: 0.05em 0.5em; border-radius: 10px; font-size: 11px; font-weight: 600; }

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

/* Continue (follow-up) — sticky at bottom with translucent backdrop */
form.continue {
  position: sticky;
  bottom: 0;
  background: rgba(10,10,12,0.85);
  -webkit-backdrop-filter: blur(10px);
  backdrop-filter: blur(10px);
  padding: 0.8em 0 1em 0;
  margin: 1.2em -2em 0;
  padding-left: 2em;
  padding-right: 2em;
  border-top: 1px solid var(--line);
  z-index: 5;
}
form.continue textarea { min-height: 2.2em; max-height: 300px; overflow-y: auto; transition: height 80ms ease-out; }
.continue-actions { display: flex; justify-content: flex-end; margin-top: 0.4em; gap: 0.5em; }

/* Custom scrollbars across the app (Webkit + Firefox) */
::-webkit-scrollbar { width: 10px; height: 10px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: var(--line); border-radius: 5px; border: 2px solid transparent; background-clip: padding-box; }
::-webkit-scrollbar-thumb:hover { background: var(--dim); background-clip: padding-box; border: 2px solid transparent; }
::-webkit-scrollbar-corner { background: transparent; }
* { scrollbar-width: thin; scrollbar-color: var(--line) transparent; }

/* Events stream — single page scroll, no nested container */
.events { display: flex; flex-direction: column; gap: 0.9em; padding-top: 0.5em; }
.turn { line-height: 1.55; }
.turn.assistant { color: var(--fg); padding: 0.2em 0; }
.turn.user { display: flex; justify-content: flex-end; }
.turn.user .user-bubble {
  background: rgba(212,180,120,0.06);
  border: 1px solid rgba(212,180,120,0.35);
  border-radius: 12px 12px 4px 12px;
  padding: 0.6em 0.95em;
  max-width: 78%;
  color: var(--fg);
}
.turn.user .user-bubble p { margin: 0; }
.turn.error { color: var(--err); padding: 0.4em 0.8em; background: rgba(220,110,90,0.06); border-left: 3px solid var(--err); border-radius: 0 4px 4px 0; }
.turn.verdict {
  padding: 0.85em 1em;
  border: 1px solid var(--line);
  border-radius: 8px;
  margin-top: 1em;
}
.turn.verdict.verdict-pass { border-color: rgba(140,180,110,0.45); background: rgba(140,180,110,0.05); }
.turn.verdict.verdict-fail { border-color: rgba(220,110,90,0.45); background: rgba(220,110,90,0.05); }
.turn.verdict.verdict-other { border-color: rgba(220,175,90,0.45); background: rgba(220,175,90,0.05); }
.turn.verdict .badge { padding: 0.15em 0.55em; border-radius: 4px; font-size: 11px; font-weight: 700; margin-right: 0.6em; text-transform: uppercase; vertical-align: middle; }
.turn.verdict .badge.verdict-pass { background: var(--add-bg); color: var(--add-fg); }
.turn.verdict .badge.verdict-fail { background: var(--rem-bg); color: var(--rem-fg); }
.turn.verdict .badge.verdict-other { background: rgba(220,175,90,0.15); color: var(--warn); }

/* Live LLM streaming with smooth fade transitions */
.turn.assistant.composing {
  white-space: pre-wrap;
  color: var(--assistant);
  min-height: 1em;
  position: relative;
  opacity: 1;
  transition: opacity 200ms ease-out;
}
.turn.assistant.composing.fade-out { opacity: 0; }
.turn.assistant.composing::after { content: '▊'; color: var(--accent); animation: blink 1s step-end infinite; margin-left: 1px; }
@keyframes blink { 50% { opacity: 0; } }

/* Thinking indicator (timer-driven) */
.turn.thinking { color: var(--dim); font-size: 12px; padding: 0.4em 0; display: flex; align-items: center; gap: 0.5em; }
.turn.thinking .dots span { display: inline-block; width: 4px; height: 4px; background: var(--accent); border-radius: 50%; margin-right: 3px; animation: think 1.4s infinite ease-in-out both; }
.turn.thinking .dots span:nth-child(1) { animation-delay: -0.32s; }
.turn.thinking .dots span:nth-child(2) { animation-delay: -0.16s; }
@keyframes think { 0%,80%,100% { transform: scale(0); } 40% { transform: scale(1); } }

/* Empty state */
.turn.empty-state { color: var(--dim); text-align: center; padding: 3em 1em; }
.turn.empty-state .glyph { font-size: 24px; color: var(--fade); margin-bottom: 0.5em; }

/* Action card — left-border accent per tool */
.turn.action {
  background: var(--panel-2);
  border: 1px solid var(--line);
  border-left-width: 3px;
  border-radius: 6px;
  padding: 0.55em 0.8em;
}
.turn.action[data-tool="shell"] { border-left-color: var(--dim); }
.turn.action[data-tool="fs_read"] { border-left-color: #7a9cb8; }
.turn.action[data-tool="fs_write"] { border-left-color: var(--accent); }
.turn.action[data-tool="fs_write"][data-new="true"] { border-left-color: var(--add-fg); }
.turn.action[data-tool="other"] { border-left-color: var(--accent); }
.turn.action.action-error { border-color: var(--err); border-left-color: var(--err); background: rgba(220,110,90,0.04); }

.action-head { display: flex; align-items: center; flex-wrap: wrap; gap: 0.5em; font-size: 12px; }
.action-tool { color: var(--accent); font-weight: 700; }
.action-verb { color: var(--heading); font-weight: 600; }
.action-cmd, .action-path { color: var(--fg); background: rgba(255,255,255,0.04); padding: 0.1em 0.45em; border-radius: 3px; word-break: break-all; }
.chip-spacer { flex: 1; }
.action-chip { font-size: 10px; padding: 0.05em 0.5em; border-radius: 10px; background: rgba(212,180,120,0.15); color: var(--accent); font-weight: 600; letter-spacing: 0.02em; }
.action-chip.muted-chip { background: rgba(255,255,255,0.04); color: var(--dim); font-weight: 400; }
.action-more { margin-top: 0.3em; }
.action-more > summary { cursor: pointer; padding: 0.15em 0; list-style: none; color: var(--dim); }
.action-more > summary:hover { color: var(--fg); }
.action-more > summary::-webkit-details-marker { display: none; }
.action-out {
  background: rgba(0,0,0,0.28);
  border: 1px solid var(--line);
  padding: 0.5em 0.7em;
  font-size: 12px;
  line-height: 1.5;
  border-radius: 4px;
  margin: 0.35em 0 0 0;
  white-space: pre-wrap;
  word-break: break-word;
}
.action-out.action-err { color: var(--err); background: rgba(220,110,90,0.06); border-color: rgba(220,110,90,0.3); }

/* Rollup card */
.rollup.card { margin: 1.2em 0 0; padding: 0; }
.rollup-head { display: flex; align-items: center; gap: 0.6em; padding: 0.6em 0.8em; border-bottom: 1px solid var(--line); }
.rollup-title { color: var(--heading); font-weight: 600; flex: 1; }
.rollup-body { padding: 0.3em 0; }
.file-row { display: grid; grid-template-columns: 60px 1fr 50px 50px; gap: 0.6em; align-items: center; padding: 0.25em 0.8em; font-size: 12px; }
.file-row:hover { background: var(--panel-2); }
.file-name { color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.file-row .add, .file-row .rem { text-align: right; font-variant-numeric: tabular-nums; }

/* Inline markdown rendering inside decision/verdict bodies */
.md p { display: block; margin: 0.3em 0; }
.md p:first-child { margin-top: 0; }
.md p:last-child { margin-bottom: 0; }
.md strong { color: var(--heading); font-weight: 700; }
.md em { font-style: italic; }
.md code { background: rgba(212,180,120,0.10); color: var(--accent); padding: 0.05em 0.35em; border-radius: 3px; font-size: 0.95em; }
.md pre { background: var(--panel-2); border: 1px solid var(--line); padding: 0.6em 0.8em; border-radius: 4px; overflow-x: auto; margin: 0.5em 0; }
.md pre code { background: transparent; color: var(--fg); padding: 0; font-size: 12px; }
.md h1, .md h2, .md h3, .md h4 { color: var(--heading); margin: 0.6em 0 0.3em; font-weight: 700; }
.md h1 { font-size: 16px; }
.md h2 { font-size: 15px; }
.md h3, .md h4 { font-size: 14px; }
.md ul, .md ol { margin: 0.4em 0 0.4em 1.6em; padding: 0; list-style-position: outside; }
.md ul li, .md ol li { margin: 0.15em 0; padding-left: 0.2em; }
.md ul { list-style: disc; }
.md ol { list-style: decimal; }
.md ul ul, .md ol ol, .md ul ol, .md ol ul { margin-left: 1.2em; margin-top: 0.1em; }
.md blockquote { border-left: 2px solid var(--line); margin: 0.4em 0; padding: 0.1em 0.8em; color: var(--dim); }
.md blockquote { border-left: 2px solid var(--line); margin: 0.4em 0; padding: 0.1em 0.8em; color: var(--dim); display: block; font-style: normal; }
.md a { color: var(--link); text-decoration: underline; }
.md hr { border: none; border-top: 1px solid var(--line); margin: 0.8em 0; }
.md table { border-collapse: collapse; margin: 0.5em 0; display: block; }
.md th, .md td { border: 1px solid var(--line); padding: 0.3em 0.6em; }
.md th { background: var(--panel-2); color: var(--heading); }
.evt.evt-tool_call .tool { color: var(--accent); }
.evt.evt-tool_call .args { color: var(--dim); }
.evt.evt-tool_result .body { color: var(--dim); }
.evt.evt-verdict .body { font-weight: 600; }

/* Diff body (used in action-detail) */
pre.diff-body { background: rgba(0,0,0,0.3); border: 1px solid var(--line); padding: 0.5em 0.7em; font-size: 12px; line-height: 1.45; overflow-x: auto; margin: 0.3em 0 0 0; border-radius: 4px; max-height: 320px; overflow-y: auto; }
.badge.new { background: var(--add-bg); color: var(--add-fg); padding: 0.05em 0.4em; font-size: 10px; font-weight: 600; border-radius: 3px; text-transform: uppercase; }
.badge.edit { background: rgba(212,180,120,0.15); color: var(--accent); padding: 0.05em 0.4em; font-size: 10px; font-weight: 600; border-radius: 3px; text-transform: uppercase; }
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
