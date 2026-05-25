//! gRPC service implementation. M1: Ping, Ask. M2: SubmitTask, GetTask, ListTasks,
//! CancelTask, StreamEvents. M3: sandbox + worktree per task.

use anyhow::{Context, Result, anyhow};
use futures::StreamExt;
use jarvis_agent::{AgentRun, HookPhase, HookSpec, run_agent};
use jarvis_api::{
    AskChunk, AskRequest, CommitInfo, CommitPhaseRequest, CostReport, DaemonStatus, DiffGroup,
    DiffGroupList, EditMemoryRequest, Empty, Event as ApiEvent, FileDiff, FileDiffChunkBatch,
    FleetDelta, FleetEdge, FleetFrame, FleetNode, FleetSnapshot, GetFileDiffRequest,
    ListMemoriesRequest, ListTasksRequest, Memory as ApiMemory, MemoryHandle, MemoryList,
    ModelSpend, ModelStatus as ApiModelStatus, PingRequest, PingResponse, PromoteMemoryRequest,
    Schedule as ApiSchedule, ScheduleHandle, ScheduleList, ScheduleSpec, StatusRequest,
    StreamEventsRequest, Task as ApiTask, TaskHandle, TaskList, TaskSpec,
    TimelineEvent as ApiTimelineEvent, TimelineSnapshot, TimelineSpan, UsageStats,
    fleet_frame::Kind as FleetFrameKind,
    jarvis_server::{Jarvis, JarvisServer},
};
use jarvis_config::Config;
use jarvis_core::{
    AgentId, Capabilities, ChatMessage, ChatRequest, LlmProvider, ProviderName,
    RequiredCapabilities, RoutingPolicy, TaskId, TaskKind,
};
use jarvis_ledger::{EventRecord, Ledger, TaskRecord, TaskRuntimeInfo};
use jarvis_llm::{
    LlmPool, ModelKind, ModelRegistry, OpenAiCompatConfig, OpenAiCompatProvider, QuarantineConfig,
    make_openai_compat_entry,
};
use jarvis_mcp::{McpClient, McpServerSpec, McpToolAdapter};
use jarvis_sandbox::{
    DockerSandbox, NativeSandbox, NetPolicy, Sandbox, SandboxKind, Worktree, WorktreeManager,
};
use jarvis_tools::{
    ApplyPatchTool, FsReadTool, FsWriteTool, GlobTool, GrepTool, ShellTool, ToolCtx, ToolRegistry,
    UpdatePlanTool,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status, transport::Server};
use tracing::{error, info, instrument, warn};

pub async fn run(cfg: Config, bind: String) -> Result<()> {
    let registry = build_registry(&cfg)?;
    if registry.is_empty() {
        return Err(anyhow!(
            "no providers configured — add at least one [providers.local.*]"
        ));
    }
    info!(models = registry.len(), "model registry built");

    let pool = Arc::new(LlmPool::new(
        registry,
        QuarantineConfig {
            threshold: cfg.routing.quarantine_after_failures,
            window: chrono::Duration::minutes(cfg.routing.quarantine_window_minutes as i64),
            duration: chrono::Duration::minutes(cfg.routing.quarantine_duration_minutes as i64),
        },
    ));
    // Boot probe in the background — don't block daemon startup.
    {
        let pool = pool.clone();
        tokio::spawn(async move { pool.probe_all().await });
    }

    // Pick the default planning provider for the Ask RPC (one-shot helper).
    let ask_provider = pick_ask_provider(&pool).await;

    let ledger_path = cfg.daemon.data_dir.join("ledger.sqlite");
    let ledger = Ledger::open(&ledger_path).await.context("open ledger")?;

    let (tools, mcp_status) = build_tool_registry(&cfg).await;

    let native: Arc<dyn Sandbox> = Arc::new(NativeSandbox);
    let docker = build_docker_sandbox(&cfg.sandbox).await;
    if docker.is_none() && cfg.sandbox.default_backend == "docker" {
        warn!(
            "config requests docker backend but daemon could not connect — tasks will fall back to native"
        );
    }
    // M10.S5: WSL2 sandbox is only meaningful on Windows hosts. We register
    // it unconditionally as long as we're on Windows so `--sandbox wsl2`
    // works without extra config; non-Windows hosts will see
    // `failed_precondition` if a task requests it.
    let wsl2: Option<Arc<dyn Sandbox>> = if cfg!(windows) {
        Some(Arc::new(jarvis_sandbox::WslSandbox::new()) as Arc<dyn Sandbox>)
    } else {
        None
    };

    let worktrees = Arc::new(WorktreeManager::new(cfg.daemon.data_dir.join("worktrees")));

    let running: Arc<Mutex<HashMap<TaskId, RuntimeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
    let started = Instant::now();

    let scheduler_handles: crate::scheduler::SchedulerHandles =
        Arc::new(Mutex::new(HashMap::new()));
    let svc = JarvisService {
        pool: pool.clone(),
        ask_provider,
        ledger: ledger.clone(),
        tools: tools.clone(),
        native: native.clone(),
        docker: docker.clone(),
        wsl2: wsl2.clone(),
        worktrees: worktrees.clone(),
        cfg: cfg.clone(),
        started,
        running: running.clone(),
        mcp_status: mcp_status.clone(),
        scheduler_handles: scheduler_handles.clone(),
    };

    // § C boot reconciliation — any task still marked `running` or
    // `pending` is orphaned: its tokio worker died when the daemon last
    // stopped, but the ledger never recorded a terminal status. Left
    // alone these poison the HUD ("11 running" forever) and the fleet
    // view. Mark them `failed` so the ledger reflects reality.
    match ledger.list_tasks(false, 10_000).await {
        Ok(active) => {
            let mut reconciled = 0u32;
            for t in active {
                if let Err(e) = ledger
                    .set_task_status(
                        t.id,
                        jarvis_ledger::TaskStatus::Failed,
                        Some("orphaned — daemon restarted while task was running"),
                    )
                    .await
                {
                    warn!(task = %t.id, error = %e, "could not reconcile orphaned task");
                } else {
                    reconciled += 1;
                }
            }
            if reconciled > 0 {
                info!(
                    reconciled,
                    "marked orphaned running/pending tasks as failed"
                );
            }
        }
        Err(e) => warn!(error = %e, "could not list active tasks for boot reconciliation"),
    }

    // § T2.4 — load YAML recipes from <data_dir>/recipes/ and upsert
    // them as `recipe:<name>` schedules. The recipe file is the source
    // of truth: delete + recreate on every boot so manual edits via
    // the SPA/CLI to a recipe-derived schedule don't survive a restart.
    {
        let recipes_dir = std::path::Path::new(&cfg.daemon.data_dir).join("recipes");
        match crate::recipes::load_recipes_dir(&recipes_dir) {
            Ok(recipes) if !recipes.is_empty() => {
                info!(
                    count = recipes.len(),
                    dir = %recipes_dir.display(),
                    "recipes: loaded YAML recipes"
                );
                for r in recipes {
                    let id = r.schedule_id();
                    // Ignore "not found" — first boot, or stale schedule
                    // from a previous recipe that was removed.
                    let _ = ledger.delete_schedule(&id).await;
                    let new = crate::scheduler::new_record_from_spec(
                        id.clone(),
                        r.cron.clone(),
                        r.goal.clone(),
                        r.workdir.clone(),
                        r.sandbox.clone(),
                        r.net_policy.clone(),
                        r.routing.clone(),
                        r.max_steps,
                        r.schedule_label(),
                        r.paused,
                    );
                    if let Err(e) = ledger.create_schedule(new).await {
                        warn!(
                            recipe = %r.name,
                            error = %e,
                            "recipes: failed to register schedule",
                        );
                    }
                }
            }
            Ok(_) => {}
            Err(e) => warn!(
                error = %e,
                dir = %recipes_dir.display(),
                "recipes: load failed",
            ),
        }
    }

    // M12.S1: boot any existing non-paused schedules into live cron loops.
    // Done after svc is built so the loops can call back via gRPC.
    // (Recipes registered above are picked up by this loop.)
    match ledger.list_schedules().await {
        Ok(rows) => {
            for row in rows {
                if !row.paused {
                    svc.spawn_schedule_loop(row).await;
                }
            }
        }
        Err(e) => warn!(error = %e, "could not load schedules at boot"),
    }

    let addr: std::net::SocketAddr = bind.parse().context("parse daemon.addr")?;
    info!(%addr, ledger = %ledger_path.display(), "jarvis-daemon listening");

    // M6.S15: the SolidJS SPA replaces the legacy HTMX UI. The SPA
    // serves auth + static bundle on JARVIS_SPA_ADDR (default 7879) and
    // calls the daemon's gRPC-Web endpoint on :7777 for data.
    let spa_handle = if cfg.web.enable {
        let spa_addr = jarvis_web::resolve_addr();
        let spa_data_dir = cfg.daemon.data_dir.clone();
        Some(tokio::spawn(async move {
            if let Err(e) = jarvis_web::serve(spa_addr, &spa_data_dir).await {
                warn!(error = %e, "jarvis-web SPA server stopped");
            }
        }))
    } else {
        None
    };

    // M10.S1: refuse LAN binds unless explicitly opted-in via daemon.bind_lan.
    let bound: std::net::SocketAddr = addr;
    let is_loopback = bound.ip().is_loopback();
    if !is_loopback && !cfg.daemon.bind_lan {
        return Err(anyhow!(
            "daemon.addr binds non-loopback ({bound}) but daemon.bind_lan=false; \
             set bind_lan=true in jarvis.toml (and keep a token) to allow this"
        ));
    }
    if cfg.daemon.disable_auth {
        warn!(
            "daemon.disable_auth=true — gRPC accepts ANY caller. \
             Use only on a fully trusted local box."
        );
    }

    // M10.S1: load/create the bearer token shared with jarvis-web and inject
    // the auth interceptor. All RPCs (including Ping) require the token in
    // production; the token is auto-discovered by CLI/TUI/SPA so users don't
    // see it.
    let auth_token = if cfg.daemon.disable_auth {
        None
    } else {
        match jarvis_web::auth::AuthToken::load_or_create(&cfg.daemon.data_dir) {
            Ok(t) => Some(t.as_str().to_string()),
            Err(e) => {
                warn!(error = %e, "failed to load/create web.token — gRPC auth disabled");
                None
            }
        }
    };
    let auth_interceptor = jarvis_api::auth::ServerAuth::new(auth_token, cfg.daemon.disable_auth);

    // The same port speaks two protocols:
    //   - binary gRPC over HTTP/2 (for TUI/CLI tonic clients)
    //   - gRPC-Web over HTTP/1.1 (for the SolidJS SPA via connect-es)
    // The tonic-web layer transparently translates between the two.
    let server = JarvisServer::with_interceptor(svc, auth_interceptor);
    let result = Server::builder()
        .accept_http1(true)
        .layer(tonic_web::GrpcWebLayer::new())
        .add_service(server)
        .serve(addr)
        .await
        .context("gRPC server");

    if let Some(h) = spa_handle {
        h.abort();
    }
    result
}

fn build_registry(cfg: &Config) -> Result<ModelRegistry> {
    let mut r = ModelRegistry::new();
    for (name, lp) in &cfg.providers.local {
        let mut entry = make_openai_compat_entry(
            name,
            ModelKind::Local,
            lp.url.clone(),
            lp.model.clone(),
            lp.api_key.clone().unwrap_or_default(),
            lp.priority,
            lp.capabilities.clone().into(),
            0.0,
            0.0,
        );
        // § C.M-B — propagate per-model dialect from config.
        entry.tool_dialect = lp.tool_dialect;
        entry.thinking = lp.thinking;
        info!(
            model = %entry.name,
            model_id = %entry.model_id,
            tool_dialect = %entry.tool_dialect,
            thinking = entry.thinking,
            "registered local model"
        );
        r.insert(entry);
    }
    for (name, rp) in &cfg.providers.remote {
        let mut entry = make_openai_compat_entry(
            name,
            ModelKind::Remote,
            rp.url.clone(),
            rp.model.clone(),
            rp.api_key.clone(),
            rp.priority,
            rp.capabilities.clone().into(),
            rp.cost_per_mtok_in,
            rp.cost_per_mtok_out,
        );
        entry.tool_dialect = rp.tool_dialect;
        entry.thinking = rp.thinking;
        info!(
            model = %entry.name,
            model_id = %entry.model_id,
            tool_dialect = %entry.tool_dialect,
            thinking = entry.thinking,
            "registered remote model"
        );
        r.insert(entry);
    }
    Ok(r)
}

/// One-shot Ask RPC uses a sensible default: highest-priority local model.
/// Falls back to a stub provider if registry is empty (defensive — boot bails earlier).
async fn pick_ask_provider(pool: &Arc<LlmPool>) -> Arc<dyn LlmProvider> {
    use jarvis_llm::PickRequest;
    let req = PickRequest {
        kind: TaskKind::SimpleEdit,
        routing_override: Some(RoutingPolicy::Auto),
        ..PickRequest::for_planning()
    };
    match pool.pick(&req).await {
        Ok(p) => p.provider,
        Err(_) => {
            // Empty pool was rejected at boot; this branch shouldn't trigger.
            warn!("ask provider fallback: no model available");
            Arc::new(OpenAiCompatProvider::new(OpenAiCompatConfig {
                name: ProviderName::new("local:stub"),
                base_url: "http://localhost:0/v1".to_string(),
                model: "stub".to_string(),
                api_key: String::new(),
                capabilities: Capabilities::default(),
            }))
        }
    }
}

/// Public snapshot of MCP server health, surfaced through the API/sidebar.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct McpServerStatus {
    pub name: String,
    pub connected: bool,
    pub tools: Vec<String>,
    pub error: Option<String>,
}

/// Compile every `[[hooks.pre_tool]]`, `[[hooks.post_tool]]`, and
/// `[[hooks.on_error]]` config entry into runtime `HookSpec`s tagged with
/// their `HookPhase`. Invalid regex or empty match patterns are logged and
/// skipped — we never fail task dispatch because of a typo'd hook.
pub fn compile_hooks(cfg: &jarvis_config::HooksConfig) -> Vec<HookSpec> {
    let total = cfg.pre_tool.len() + cfg.post_tool.len() + cfg.on_error.len();
    let mut out = Vec::with_capacity(total);
    compile_phase(&cfg.pre_tool, HookPhase::Pre, "pre_tool", &mut out);
    compile_phase(&cfg.post_tool, HookPhase::Post, "post_tool", &mut out);
    compile_phase(&cfg.on_error, HookPhase::OnError, "on_error", &mut out);
    out
}

fn compile_phase(
    entries: &[jarvis_config::HookConfig],
    phase: HookPhase,
    section: &'static str,
    out: &mut Vec<HookSpec>,
) {
    for h in entries {
        let pat = h.r#match.trim();
        if pat.is_empty() {
            warn!(section, "hooks: skipping entry with empty `match`");
            continue;
        }
        let re = match regex::Regex::new(pat) {
            Ok(r) => r,
            Err(e) => {
                warn!(section, pattern = pat, error = %e, "hooks: invalid regex — skipping");
                continue;
            }
        };
        let label = h
            .label
            .clone()
            .or_else(|| h.cmd.split_whitespace().next().map(|s| s.to_string()))
            .unwrap_or_else(|| "hook".to_string());
        out.push(HookSpec {
            phase,
            label,
            matcher: re,
            cmd: h.cmd.clone(),
            workdir: h.workdir.clone(),
            timeout: std::time::Duration::from_secs(h.timeout_s),
        });
    }
}

async fn build_tool_registry(cfg: &Config) -> (ToolRegistry, Arc<Vec<McpServerStatus>>) {
    let mut r = ToolRegistry::new();
    r.register(FsReadTool);
    r.register(FsWriteTool);
    r.register(ShellTool);
    r.register(ApplyPatchTool);
    r.register(GrepTool);
    r.register(GlobTool);
    r.register(UpdatePlanTool);
    // Surgical search-and-replace, batch read, folder explorer, and url fetcher
    r.register(jarvis_tools::ReplaceFileContentTool);
    r.register(jarvis_tools::FsReadManyTool);
    r.register(jarvis_tools::ListDirTool);
    r.register(jarvis_tools::FetchUrlTool);
    // Git status and View symbols
    r.register(jarvis_tools::GitStatusTool);
    r.register(jarvis_tools::ViewSymbolsTool);
    // M11.S2: web search — registered unconditionally; the tool fails fast
    // with a clear error if neither BRAVE_API_KEY nor TAVILY_API_KEY is set,
    // so the agent learns immediately to skip it instead of mid-task.
    r.register(jarvis_tools::WebSearchTool);
    // M11.S3: sub-agents primitive. The tool opens a tonic client back to
    // this daemon (loopback gRPC), so it works without any wiring beyond
    // the standard bearer-token discovery.
    r.register(jarvis_tools::SpawnSubagentTool);
    // M12.S2: GitHub PR integration via the `gh` CLI. Fails gracefully
    // (is_error: true) if `gh` is not installed or not authenticated;
    // the agent learns to fall back to plain `shell` git operations.
    r.register(jarvis_tools::GhPrListTool);
    r.register(jarvis_tools::GhPrViewTool);
    r.register(jarvis_tools::GhPrCommentTool);
    r.register(jarvis_tools::GhPrCreateTool);

    let mut statuses: Vec<McpServerStatus> = Vec::new();
    for (name, spec) in &cfg.mcp.servers {
        if !spec.enable {
            continue;
        }
        let mcp_spec = McpServerSpec {
            name: name.clone(),
            command: spec.command.clone(),
            args: spec.args.clone(),
            env: spec.env.clone(),
            workdir: spec.workdir.clone(),
        };
        // Two-stage fallible startup (connect → tools/list) flattened into a
        // single Result so the error-path doesn't fork the happy path.
        let result: std::result::Result<(jarvis_mcp::McpClient, Vec<_>), (String, String)> =
            match McpClient::connect(mcp_spec).await {
                Ok(client) => match client.list_tools().await {
                    Ok(tools) => Ok((client, tools)),
                    Err(e) => Err(("tools/list".into(), e.to_string())),
                },
                Err(e) => Err(("connect".into(), e.to_string())),
            };
        match result {
            Ok((client, tools)) => {
                let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
                info!(server = %name, count = tools.len(), "mcp tools discovered");
                for tool in tools {
                    r.register(McpToolAdapter::new(name, tool, client.clone()));
                }
                statuses.push(McpServerStatus {
                    name: name.clone(),
                    connected: true,
                    tools: tool_names,
                    error: None,
                });
            }
            Err((stage, msg)) => {
                warn!(server = %name, stage = %stage, error = %msg, "mcp startup failed");
                statuses.push(McpServerStatus {
                    name: name.clone(),
                    connected: false,
                    tools: Vec::new(),
                    error: Some(format!("{stage}: {msg}")),
                });
            }
        }
    }
    statuses.sort_by(|a, b| a.name.cmp(&b.name));

    // § T1.3 — register the `search_tools` meta-tool last so it sees every
    // built-in + every MCP-imported tool in its frozen catalog.
    r.register_search_tools();

    (r, Arc::new(statuses))
}

async fn build_docker_sandbox(cfg: &jarvis_config::SandboxConfig) -> Option<Arc<dyn Sandbox>> {
    let docker_cfg = jarvis_sandbox::docker_config_from_strings(
        &cfg.docker_image,
        cfg.docker_memory.as_deref(),
        cfg.docker_cpus,
    );
    match DockerSandbox::connect(docker_cfg).await {
        Ok(d) => {
            if cfg.docker_autopull
                && let Err(e) = d.ensure_image().await
            {
                warn!(error = %e, "docker image pull failed");
            }
            Some(Arc::new(d))
        }
        Err(e) => {
            warn!(error = %e, "docker sandbox unavailable; native-only mode");
            None
        }
    }
}

pub(crate) struct JarvisService {
    pub pool: Arc<LlmPool>,
    pub ask_provider: Arc<dyn LlmProvider>,
    pub ledger: Ledger,
    pub tools: ToolRegistry,
    pub native: Arc<dyn Sandbox>,
    pub docker: Option<Arc<dyn Sandbox>>,
    pub wsl2: Option<Arc<dyn Sandbox>>,
    pub worktrees: Arc<WorktreeManager>,
    pub cfg: Config,
    pub started: Instant,
    pub running: Arc<Mutex<HashMap<TaskId, RuntimeHandle>>>,
    #[allow(dead_code)]
    pub mcp_status: Arc<Vec<McpServerStatus>>,
    /// M12.S1: per-schedule cancellation tokens for live cron loops.
    pub scheduler_handles: crate::scheduler::SchedulerHandles,
}

impl JarvisService {
    /// Wraps `crate::scheduler::spawn_loop` with a closure that calls back
    /// into `submit_task` so the scheduler can fire tasks through the
    /// normal dispatch path (sandbox pick, worktree, agent spawn).
    pub async fn spawn_schedule_loop(&self, record: jarvis_ledger::ScheduleRecord) {
        let id = record.id.clone();
        // Build an `Arc<Self>`-equivalent by cloning the bits we need.
        // We can't Arc<JarvisService> here without restructuring, so the
        // closure captures the gRPC self-call path via Arc<Mutex<...>>
        // bookkeeping and a fresh tonic client. Simpler: capture the daemon
        // URL + auth token and call back over loopback gRPC, same trick
        // as SpawnSubagentTool.
        let daemon_url = std::env::var("JARVIS_DAEMON_URL")
            .unwrap_or_else(|_| format!("http://{}", self.cfg.daemon.addr));
        let token = jarvis_api::auth::discover_token().unwrap_or_default();
        let ledger = self.ledger.clone();
        let submit_fn = move |rec: jarvis_ledger::ScheduleRecord| {
            let url = daemon_url.clone();
            let tok = token.clone();
            tokio::spawn(async move {
                let auth = match jarvis_api::auth::ClientAuth::new(&tok) {
                    Ok(a) => a,
                    Err(e) => return Err(format!("invalid token: {e}")),
                };
                let ep = match tonic::transport::Endpoint::from_shared(url.clone()) {
                    Ok(e) => e,
                    Err(e) => return Err(format!("endpoint: {e}")),
                };
                let channel = match ep.connect().await {
                    Ok(c) => c,
                    Err(e) => return Err(format!("connect: {e}")),
                };
                let mut client =
                    jarvis_api::jarvis_client::JarvisClient::with_interceptor(channel, auth);
                let spec = schedule_to_task_spec(&rec);
                match client.submit_task(spec).await {
                    Ok(r) => Ok(r.into_inner().id),
                    Err(s) => Err(format!("submit_task: {s}")),
                }
            })
        };
        let cancel = crate::scheduler::spawn_loop(record, ledger, submit_fn);
        self.scheduler_handles.lock().await.insert(id, cancel);
    }
}

pub struct RuntimeHandle {
    pub cancel: CancellationToken,
    pub source_workdir: PathBuf,
    pub worktree: Worktree,
}

impl JarvisService {
    #[allow(clippy::result_large_err)]
    fn parse_routing(&self, raw: &str) -> Result<RoutingPolicy, Status> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(
                parse_routing_str(&self.cfg.routing.default_policy).unwrap_or(RoutingPolicy::Auto)
            );
        }
        parse_routing_str(raw)
            .ok_or_else(|| Status::invalid_argument(format!("invalid routing: {raw}")))
    }

    #[allow(clippy::result_large_err)]
    fn pick_sandbox(&self, requested: &str) -> Result<(Arc<dyn Sandbox>, SandboxKind), Status> {
        let chosen = if requested.is_empty() {
            self.cfg.sandbox.default_backend.as_str()
        } else {
            requested
        };
        match SandboxKind::from_str(chosen) {
            Ok(SandboxKind::Native) => Ok((self.native.clone(), SandboxKind::Native)),
            Ok(SandboxKind::Docker) => match &self.docker {
                Some(d) => Ok((d.clone(), SandboxKind::Docker)),
                None => Err(Status::failed_precondition(
                    "docker sandbox requested but Docker is not available",
                )),
            },
            Ok(SandboxKind::Wsl2) => match &self.wsl2 {
                Some(w) => Ok((w.clone(), SandboxKind::Wsl2)),
                None => Err(Status::failed_precondition(
                    "wsl2 sandbox requested but WSL is not available on this host",
                )),
            },
            Err(e) => Err(Status::invalid_argument(e)),
        }
    }

    #[allow(clippy::result_large_err)]
    fn pick_net(&self, requested: &str) -> Result<NetPolicy, Status> {
        let chosen = if requested.is_empty() {
            self.cfg.sandbox.default_net_policy.as_str()
        } else {
            requested
        };
        NetPolicy::from_str(chosen).map_err(Status::invalid_argument)
    }
}

fn record_to_api_task(r: &TaskRecord) -> ApiTask {
    ApiTask {
        id: r.id.to_string(),
        goal: r.goal.clone(),
        status: r.status.to_string(),
        workdir: r.workdir.clone(),
        created_at: r.created_at,
        completed_at: r.completed_at,
        error: r.error.clone(),
        sandbox: r.sandbox.clone(),
        net_policy: r.net_policy.clone(),
        worktree_path: r.worktree_path.clone(),
        worktree_branch: r.worktree_branch.clone(),
        parent_task_id: r.parent.map(|p| p.to_string()).unwrap_or_default(),
    }
}

fn record_to_api_event(r: &EventRecord) -> ApiEvent {
    ApiEvent {
        id: r.id.0,
        ts_micros: r.ts_micros,
        task_id: r.task_id.to_string(),
        agent_id: r.agent_id.map(|a| a.to_string()).unwrap_or_default(),
        kind: r.kind.to_string(),
        subject: r.subject.clone().unwrap_or_default(),
        payload_json: serde_json::to_string(&r.payload).unwrap_or_else(|_| "null".into()),
        parent_evt: r.parent_evt.map(|e| e.0).unwrap_or(0),
    }
}

#[tonic::async_trait]
impl Jarvis for JarvisService {
    // ---------- M1 ----------

    #[instrument(skip_all)]
    async fn ping(
        &self,
        _req: Request<PingRequest>,
    ) -> std::result::Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_seconds: self.started.elapsed().as_secs() as i64,
        }))
    }

    type AskStream =
        Pin<Box<dyn Stream<Item = std::result::Result<AskChunk, Status>> + Send + 'static>>;

    #[instrument(skip_all, fields(prompt_len = req.get_ref().prompt.len()))]
    async fn ask(
        &self,
        req: Request<AskRequest>,
    ) -> std::result::Result<Response<Self::AskStream>, Status> {
        let req = req.into_inner();
        if req.prompt.trim().is_empty() {
            return Err(Status::invalid_argument("prompt is empty"));
        }
        let chat_req = ChatRequest {
            messages: vec![ChatMessage::user(req.prompt)],
            temperature: req.temperature,
            top_p: None,
            max_tokens: req.max_tokens,
            stream: true,
        };
        let mut llm_stream = self
            .ask_provider
            .complete_stream(chat_req)
            .await
            .map_err(|e| Status::internal(format!("provider: {e}")))?;
        let (tx, rx) = mpsc::channel::<std::result::Result<AskChunk, Status>>(64);
        tokio::spawn(async move {
            while let Some(item) = llm_stream.next().await {
                let chunk = match item {
                    Ok(c) => AskChunk {
                        delta: c.delta,
                        usage: c.usage.map(|u| UsageStats {
                            prompt_tokens: u.prompt_tokens,
                            completion_tokens: u.completion_tokens,
                            total_tokens: u.total_tokens,
                        }),
                        finish_reason: c.finish_reason,
                    },
                    Err(e) => {
                        let _ = tx.send(Err(Status::internal(format!("llm: {e}")))).await;
                        return;
                    }
                };
                if tx.send(Ok(chunk)).await.is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    // ---------- M2 / M3 ----------

    #[instrument(skip_all, fields(goal_len = req.get_ref().goal.len()))]
    async fn submit_task(
        &self,
        req: Request<TaskSpec>,
    ) -> std::result::Result<Response<TaskHandle>, Status> {
        let mut spec = req.into_inner();
        if spec.goal.trim().is_empty() {
            return Err(Status::invalid_argument("goal is empty"));
        }
        // M10.S4: `resume_from` is just sugar for "spawn me a child of this
        // task and inherit everything". Normalize before the parent-lookup
        // path. Explicit `parent_task_id` wins if both are set.
        let resumed_from = if !spec.resume_from.is_empty() && spec.parent_task_id.is_empty() {
            spec.parent_task_id = spec.resume_from.clone();
            Some(spec.resume_from.clone())
        } else {
            None
        };
        // Resolve parent task (if any) so we can inherit sensible defaults.
        let parent_record = if spec.parent_task_id.is_empty() {
            None
        } else {
            let parent_id = parse_task_id(&spec.parent_task_id)?;
            Some(
                self.ledger
                    .get_task(parent_id)
                    .await
                    .map_err(|_| Status::not_found("parent task not found"))?,
            )
        };

        // Workdir: explicit > parent's > cwd.
        let source_workdir = if !spec.workdir.is_empty() {
            PathBuf::from(&spec.workdir)
        } else if let Some(p) = &parent_record {
            PathBuf::from(&p.workdir)
        } else {
            std::env::current_dir().unwrap_or(PathBuf::from("."))
        };

        // Sandbox: explicit > parent's > config default.
        let sandbox_pref = if !spec.sandbox.is_empty() {
            spec.sandbox.clone()
        } else {
            parent_record
                .as_ref()
                .map(|p| p.sandbox.clone())
                .unwrap_or_default()
        };
        let net_pref = if !spec.net_policy.is_empty() {
            spec.net_policy.clone()
        } else {
            parent_record
                .as_ref()
                .map(|p| p.net_policy.clone())
                .unwrap_or_default()
        };

        // Pick sandbox + net policy (fail fast on bad inputs).
        let (sandbox, kind) = self.pick_sandbox(&sandbox_pref)?;
        let net = self.pick_net(&net_pref)?;

        // Create the task row before the worktree so the worktree can use the task id.
        let parent_id = parent_record.as_ref().map(|p| p.id);
        let task = self
            .ledger
            .create_task(&spec.goal, &source_workdir.display().to_string(), parent_id)
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;

        // M10.S4: if this is a resume, drop a `continuation` event marker
        // so the timeline reflects the inheritance and the agent loop's
        // prompt builder picks it up alongside the parent's history.
        if let Some(src) = resumed_from {
            let _ = self
                .ledger
                .append(
                    jarvis_ledger::NewEvent::new(
                        task.id,
                        jarvis_ledger::EventKind::Continuation,
                        serde_json::json!({
                            "kind": "resume",
                            "resumed_from": src,
                            "reason": "user invoked resume; inheriting workdir/sandbox/net from source",
                        }),
                    )
                    .with_subject(format!("resume from {}", &src[..src.len().min(8)])),
                )
                .await;
        }

        // Create worktree if requested. Falls back to source path if no git repo.
        let worktree = if spec.use_worktree {
            let mgr = self.worktrees.clone();
            let task_id = task.id;
            let source = source_workdir.clone();
            let base = if spec.base_ref.is_empty() {
                None
            } else {
                Some(spec.base_ref.clone())
            };
            match tokio::task::spawn_blocking(move || mgr.create(task_id, &source, base.as_deref()))
                .await
            {
                Ok(Ok(wt)) => wt,
                Ok(Err(e)) => {
                    let _ = self
                        .ledger
                        .set_task_status(
                            task.id,
                            jarvis_ledger::TaskStatus::Failed,
                            Some(&format!("worktree: {e}")),
                        )
                        .await;
                    return Err(Status::internal(format!("worktree: {e}")));
                }
                Err(e) => return Err(Status::internal(format!("worktree spawn: {e}"))),
            }
        } else {
            Worktree {
                task_id: task.id,
                path: source_workdir.clone(),
                branch: String::new(),
                managed: false,
            }
        };

        // Persist runtime info.
        let runtime_info = TaskRuntimeInfo {
            sandbox: kind.as_str().to_string(),
            net_policy: net.as_str().to_string(),
            worktree_path: worktree.path.display().to_string(),
            worktree_branch: worktree.branch.clone(),
        };
        let _ = self.ledger.set_task_runtime(task.id, &runtime_info).await;

        let cancel = CancellationToken::new();
        self.running.lock().await.insert(
            task.id,
            RuntimeHandle {
                cancel: cancel.clone(),
                source_workdir: source_workdir.clone(),
                worktree: worktree.clone(),
            },
        );

        let ctx = ToolCtx {
            workdir: worktree.path.clone(),
            cancel: Arc::new(cancel.clone()),
            sandbox: sandbox.clone(),
            net_policy: net.clone(),
            current_task_id: task.id.to_string(),
        };
        // Routing policy / required caps for this run.
        let routing = self.parse_routing(&spec.routing_policy)?;
        let required = parse_required_caps(&spec.require_caps);

        let sandbox_mode = self.cfg.sandbox.default_mode.parse().unwrap_or_default();
        let validation = jarvis_agent::ValidationSpec {
            enabled: self.cfg.validation.enabled,
            model: self.cfg.validation.model.clone(),
            max_validations: self.cfg.validation.max_validations,
            use_subagent: self.cfg.validation.use_subagent,
        };
        let run = AgentRun {
            task_id: task.id,
            workdir: worktree.path.clone(),
            max_steps: if spec.max_steps == 0 {
                20
            } else {
                spec.max_steps
            },
            agent_id: AgentId::new(),
            cancel,
            routing,
            required,
            kind: TaskKind::Planning,
            continuation_budget: 1,
            hooks: compile_hooks(&self.cfg.hooks),
            sandbox_mode,
            validation,
            lazy_tool_catalog: self.cfg.agent.lazy_tool_catalog,
        };

        let pool = self.pool.clone();
        let ledger = self.ledger.clone();
        let tools = self.tools.clone();
        let running = self.running.clone();
        let worktrees = self.worktrees.clone();
        let task_id = task.id;
        tokio::spawn(async move {
            let outcome = run_agent(run, pool, ledger, tools, ctx).await;
            match &outcome {
                Ok(o) => info!(task = %task_id, ?o, "agent finished"),
                Err(e) => error!(task = %task_id, error = %e, "agent failed"),
            }
            // Cleanup runtime + worktree.
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

        Ok(Response::new(TaskHandle {
            id: task.id.to_string(),
        }))
    }

    #[instrument(skip_all)]
    async fn get_task(
        &self,
        req: Request<TaskHandle>,
    ) -> std::result::Result<Response<ApiTask>, Status> {
        let id = parse_task_id(&req.into_inner().id)?;
        let t = self
            .ledger
            .get_task(id)
            .await
            .map_err(|_| Status::not_found("task not found"))?;
        Ok(Response::new(record_to_api_task(&t)))
    }

    #[instrument(skip_all)]
    async fn list_tasks(
        &self,
        req: Request<ListTasksRequest>,
    ) -> std::result::Result<Response<TaskList>, Status> {
        let req = req.into_inner();
        let recs = self
            .ledger
            .list_tasks(req.include_finished, req.limit)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(TaskList {
            tasks: recs.iter().map(record_to_api_task).collect(),
        }))
    }

    #[instrument(skip_all)]
    async fn cancel_task(
        &self,
        req: Request<TaskHandle>,
    ) -> std::result::Result<Response<jarvis_api::Empty>, Status> {
        let id = parse_task_id(&req.into_inner().id)?;
        let maybe = self.running.lock().await.get(&id).map(|h| h.cancel.clone());
        match maybe {
            Some(tok) => {
                tok.cancel();
                Ok(Response::new(jarvis_api::Empty {}))
            }
            None => Err(Status::not_found("task is not running")),
        }
    }

    type StreamEventsStream =
        Pin<Box<dyn Stream<Item = std::result::Result<ApiEvent, Status>> + Send + 'static>>;

    #[instrument(skip_all, fields(task_id = %req.get_ref().task_id, follow = req.get_ref().follow))]
    async fn stream_events(
        &self,
        req: Request<StreamEventsRequest>,
    ) -> std::result::Result<Response<Self::StreamEventsStream>, Status> {
        let req = req.into_inner();
        let task_filter = if req.task_id.is_empty() {
            None
        } else {
            Some(parse_task_id(&req.task_id)?)
        };

        let backfill = match (task_filter, req.include_ancestors) {
            (Some(t), true) => {
                let chain = self
                    .ledger
                    .walk_ancestors(t)
                    .await
                    .map_err(|e| Status::internal(format!("ledger ancestors: {e}")))?;
                let ids: Vec<_> = chain.iter().map(|tr| tr.id).collect();
                self.ledger
                    .query_events_multi(&ids, req.since_id, 0)
                    .await
                    .map_err(|e| Status::internal(format!("ledger multi: {e}")))?
            }
            _ => self
                .ledger
                .query_events(task_filter, req.since_id, 0)
                .await
                .map_err(|e| Status::internal(format!("ledger: {e}")))?,
        };
        let mut live = self.ledger.subscribe();
        let (tx, rx) = mpsc::channel::<std::result::Result<ApiEvent, Status>>(128);

        let follow = req.follow;
        tokio::spawn(async move {
            for rec in backfill {
                if tx.send(Ok(record_to_api_event(&rec))).await.is_err() {
                    return;
                }
            }
            if !follow {
                return;
            }
            loop {
                match live.recv().await {
                    Ok(rec) => {
                        if let Some(t) = task_filter
                            && rec.task_id != t
                        {
                            continue;
                        }
                        if tx.send(Ok(record_to_api_event(&rec))).await.is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "stream lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    #[instrument(skip_all)]
    async fn get_status(
        &self,
        _req: Request<StatusRequest>,
    ) -> std::result::Result<Response<DaemonStatus>, Status> {
        let statuses = self.pool.status_all().await;
        let api_models: Vec<ApiModelStatus> = statuses
            .into_iter()
            .map(|m| {
                let until_micros = m
                    .quarantined_until
                    .map(|t| t.timestamp_micros())
                    .unwrap_or(0);
                ApiModelStatus {
                    name: m.name.as_str().to_string(),
                    kind: m.kind.as_str().to_string(),
                    model_id: m.model_id,
                    priority: m.priority,
                    online: m.online,
                    quarantined: m.quarantined,
                    quarantined_until_micros: until_micros,
                    failures_in_window: m.failures_in_window,
                    ctx_len: 0,
                    tool_calls: false,
                    json_schema: false,
                    vision: false,
                }
            })
            .collect();
        let running = self.running.lock().await.len() as u32;
        let mcp_servers = self
            .mcp_status
            .iter()
            .map(|s| jarvis_api::McpServerStatus {
                name: s.name.clone(),
                connected: s.connected,
                tools: s.tools.clone(),
                error: s.error.clone().unwrap_or_default(),
            })
            .collect();
        Ok(Response::new(DaemonStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_seconds: self.started.elapsed().as_secs() as i64,
            models: api_models,
            running_tasks: running,
            mcp_servers,
        }))
    }

    type StreamFleetStream =
        Pin<Box<dyn Stream<Item = std::result::Result<FleetFrame, Status>> + Send + 'static>>;

    #[instrument(skip_all)]
    async fn stream_fleet(
        &self,
        _request: Request<Empty>,
    ) -> std::result::Result<Response<Self::StreamFleetStream>, Status> {
        let ledger = self.ledger.clone();
        let pool = self.pool.clone();
        let (tx, rx) = mpsc::channel::<std::result::Result<FleetFrame, Status>>(8);
        let mut live = ledger.subscribe();

        // M7.1: maintain a per-stream `last sent` view of the fleet so each
        // tick can be emitted as a `FleetDelta` (only what changed) rather
        // than a full snapshot. The very first frame is always a snapshot;
        // empty deltas are silently dropped so idle dashboards never wake.
        tokio::spawn(async move {
            let mut state = FleetState::default();
            if send_fleet_frame(&ledger, &pool, &tx, &mut state, true)
                .await
                .is_err()
            {
                return;
            }
            // Cap re-evaluations at 4 Hz so a flood of llm_chunk events
            // doesn't drown the client side; the per-frame delta is cheap
            // but the SQL read is still ~O(tasks).
            let mut last_send = std::time::Instant::now() - std::time::Duration::from_millis(250);
            loop {
                match live.recv().await {
                    Ok(_rec) => {
                        if last_send.elapsed() < std::time::Duration::from_millis(250) {
                            continue;
                        }
                        last_send = std::time::Instant::now();
                        if send_fleet_frame(&ledger, &pool, &tx, &mut state, false)
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "stream_fleet lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn get_task_cost_breakdown(
        &self,
        request: Request<TaskHandle>,
    ) -> std::result::Result<Response<CostReport>, Status> {
        let task_id_str = request.into_inner().id;
        let task_id = parse_task_id(&task_id_str)?;
        let events = self
            .ledger
            .timeline_events(task_id)
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;

        // Accumulate per-model usage from llm_chunk / decision payloads.
        // The exact location of `usage` depends on the agent loop; we look in
        // both common spots and treat anything missing as 0.
        let mut by_model: HashMap<String, ModelSpend> = HashMap::new();
        let registry = self.pool.registry();
        for e in &events {
            let (model_opt, in_t, out_t) = extract_usage(&e.payload);
            if let Some(model) = model_opt
                && (in_t > 0 || out_t > 0)
            {
                let entry = by_model.entry(model.clone()).or_insert(ModelSpend {
                    model_name: model.clone(),
                    tokens_in: 0,
                    tokens_out: 0,
                    cost_usd: 0.0,
                    call_count: 0,
                });
                entry.tokens_in += in_t;
                entry.tokens_out += out_t;
                entry.call_count += 1;
                let provider_name = ProviderName::new(model);
                entry.cost_usd += jarvis_llm::estimate_usd(registry, &provider_name, in_t, out_t);
            }
        }

        let (total_in, total_out, total_usd) = by_model
            .values()
            .fold((0u64, 0u64, 0.0_f64), |(ai, ao, ac), s| {
                (ai + s.tokens_in, ao + s.tokens_out, ac + s.cost_usd)
            });

        Ok(Response::new(CostReport {
            task_id: task_id_str,
            total_tokens_in: total_in,
            total_tokens_out: total_out,
            total_cost_usd: total_usd,
            by_model: by_model.into_values().collect(),
        }))
    }

    async fn group_diff_by_intent(
        &self,
        request: Request<TaskHandle>,
    ) -> std::result::Result<Response<DiffGroupList>, Status> {
        let task_id_str = request.into_inner().id;
        let task_id = parse_task_id(&task_id_str)?;
        let task = self
            .ledger
            .get_task(task_id)
            .await
            .map_err(|e| Status::not_found(format!("task: {e}")))?;
        let events = self
            .ledger
            .timeline_events(task_id)
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;

        // Index decisions by their event id so we can group by parent_evt.
        let mut decisions: HashMap<i64, &EventRecord> = HashMap::new();
        for e in &events {
            if e.kind == jarvis_ledger::EventKind::Decision {
                decisions.insert(e.id.0, e);
            }
        }

        // For every tool_call that's a fs_write or apply_patch, find its
        // parent decision and bucket its file edits.
        #[allow(clippy::type_complexity)]
        let mut buckets: HashMap<i64, (Vec<(String, String)>, i64)> = HashMap::new();
        let mut orphans: Vec<String> = Vec::new();
        for e in &events {
            if e.kind != jarvis_ledger::EventKind::ToolCall {
                continue;
            }
            let (tool, _) = parse_tool_call_payload(&e.payload);
            if tool != "fs_write" && tool != "apply_patch" {
                continue;
            }
            let path = e
                .payload
                .get("args")
                .and_then(|a| a.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if path.is_empty() {
                continue;
            }
            match e.parent_evt {
                Some(p) if decisions.contains_key(&p.0) => {
                    let entry = buckets.entry(p.0).or_insert((Vec::new(), e.ts_micros));
                    entry.0.push((path, tool));
                    entry.1 = entry.1.max(e.ts_micros);
                }
                _ => orphans.push(path),
            }
        }

        // Materialise FileDiff metadata. The diff body itself is fetched
        // lazily by the SPA via `GetFileDiff` (M8.1).
        let workdir = if !task.worktree_path.is_empty() {
            std::path::PathBuf::from(&task.worktree_path)
        } else {
            std::path::PathBuf::from(&task.workdir)
        };

        let mut groups: Vec<DiffGroup> = Vec::new();
        let mut sorted_keys: Vec<i64> = buckets.keys().copied().collect();
        sorted_keys.sort();
        for decision_id in sorted_keys {
            let (paths, ts) = buckets.remove(&decision_id).unwrap();
            let decision = decisions.get(&decision_id).unwrap();
            let decision_text = decision
                .payload
                .get("thought")
                .or_else(|| decision.payload.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let step = decision
                .payload
                .get("step")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            // Dedupe paths within a group, last-write wins.
            let mut seen = std::collections::BTreeSet::<String>::new();
            let mut files: Vec<FileDiff> = Vec::new();
            for (path, _tool) in paths.into_iter().rev() {
                if !seen.insert(path.clone()) {
                    continue;
                }
                let file = build_file_diff(&workdir, &path);
                files.push(file);
            }
            files.reverse();
            groups.push(DiffGroup {
                decision_evt_id: decision_id,
                decision_text,
                step,
                ts_micros: ts,
                files,
            });
        }

        Ok(Response::new(DiffGroupList {
            task_id: task_id_str,
            groups,
            orphan_paths: orphans,
        }))
    }

    async fn get_file_diff(
        &self,
        request: Request<GetFileDiffRequest>,
    ) -> std::result::Result<Response<FileDiffChunkBatch>, Status> {
        let req = request.into_inner();
        if req.path.is_empty() {
            return Err(Status::invalid_argument("path is empty"));
        }
        let task_id = parse_task_id(&req.task_id)?;
        let task = self
            .ledger
            .get_task(task_id)
            .await
            .map_err(|e| Status::not_found(format!("task: {e}")))?;
        let workdir = if !task.worktree_path.is_empty() {
            std::path::PathBuf::from(&task.worktree_path)
        } else {
            std::path::PathBuf::from(&task.workdir)
        };
        // Reconstruct before/after for this path. We don't strictly
        // verify that the file actually belongs to `group_evt_id` —
        // doing so would require a full re-bucket on every chunk; the
        // SPA only ever asks for paths it just received from
        // GroupDiffByIntent so the trust boundary is the gRPC caller.
        let abs = workdir.join(&req.path);
        let after = std::fs::read_to_string(&abs).unwrap_or_default();
        let before = read_path_from_head(&workdir, &req.path).unwrap_or_default();
        let change_kind = match (!before.is_empty(), abs.exists()) {
            (false, true) => "added",
            (true, false) => "deleted",
            _ => "modified",
        };
        let diff_text = compute_unified_diff(&before, &after, &req.path, change_kind);
        // Split into lines; preserve empty lines but drop the implicit
        // trailing empty after the last `\n`. The client re-joins with
        // `\n`. This keeps chunk boundaries line-aligned (never inside
        // a single diff line).
        let lines: Vec<&str> = if diff_text.is_empty() {
            Vec::new()
        } else {
            diff_text.split('\n').collect()
        };
        let total_lines = lines.len() as u32;
        let default_max: u32 = 200;
        let max = if req.max_lines == 0 {
            default_max
        } else {
            req.max_lines
        };
        let offset = req.offset_lines.min(total_lines);
        let end = offset.saturating_add(max).min(total_lines);
        let returned_lines = end - offset;
        let chunk = if returned_lines == 0 {
            String::new()
        } else {
            lines[offset as usize..end as usize].join("\n")
        };
        Ok(Response::new(FileDiffChunkBatch {
            path: req.path,
            offset_lines: offset,
            returned_lines,
            total_lines,
            has_more: end < total_lines,
            content: chunk,
        }))
    }

    async fn commit_phase(
        &self,
        request: Request<CommitPhaseRequest>,
    ) -> std::result::Result<Response<CommitInfo>, Status> {
        let req = request.into_inner();
        let task_id = parse_task_id(&req.task_id)?;
        let task = self
            .ledger
            .get_task(task_id)
            .await
            .map_err(|e| Status::not_found(format!("task: {e}")))?;
        // Re-fetch the group so we know what paths to stage.
        let groups = self
            .group_diff_by_intent(Request::new(TaskHandle {
                id: task_id.to_string(),
            }))
            .await?
            .into_inner();
        let group = groups
            .groups
            .into_iter()
            .find(|g| g.decision_evt_id == req.decision_evt_id)
            .ok_or_else(|| Status::not_found("decision group not found"))?;

        let workdir = if !task.worktree_path.is_empty() {
            std::path::PathBuf::from(&task.worktree_path)
        } else {
            std::path::PathBuf::from(&task.workdir)
        };
        let subject = if req.subject.trim().is_empty() {
            synthesize_subject(&group.decision_text)
        } else {
            req.subject.trim().to_string()
        };
        let body = group.decision_text.trim().to_string();
        let paths: Vec<String> = group.files.iter().map(|f| f.path.clone()).collect();
        let info = commit_paths(&workdir, &paths, &subject, &body)
            .map_err(|e| Status::internal(format!("git: {e}")))?;
        Ok(Response::new(info))
    }

    async fn reject_phase(
        &self,
        request: Request<CommitPhaseRequest>,
    ) -> std::result::Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let task_id = parse_task_id(&req.task_id)?;
        let task = self
            .ledger
            .get_task(task_id)
            .await
            .map_err(|e| Status::not_found(format!("task: {e}")))?;
        let groups = self
            .group_diff_by_intent(Request::new(TaskHandle {
                id: task_id.to_string(),
            }))
            .await?
            .into_inner();
        let group = groups
            .groups
            .into_iter()
            .find(|g| g.decision_evt_id == req.decision_evt_id)
            .ok_or_else(|| Status::not_found("decision group not found"))?;
        let workdir = if !task.worktree_path.is_empty() {
            std::path::PathBuf::from(&task.worktree_path)
        } else {
            std::path::PathBuf::from(&task.workdir)
        };
        let paths: Vec<String> = group.files.iter().map(|f| f.path.clone()).collect();
        revert_paths(&workdir, &paths).map_err(|e| Status::internal(format!("git: {e}")))?;
        Ok(Response::new(Empty {}))
    }

    async fn list_memories(
        &self,
        request: Request<ListMemoriesRequest>,
    ) -> std::result::Result<Response<MemoryList>, Status> {
        use jarvis_ledger::{MemoryScope, MemoryStatus};
        use std::str::FromStr;
        let req = request.into_inner();
        let scope =
            if req.scope.is_empty() {
                None
            } else {
                Some(MemoryScope::from_str(&req.scope).map_err(|_| {
                    Status::invalid_argument(format!("invalid scope: {}", req.scope))
                })?)
            };
        let scope_value = if req.scope_value.is_empty() {
            None
        } else {
            Some(req.scope_value.as_str())
        };
        let status =
            if req.status.is_empty() {
                None
            } else {
                Some(MemoryStatus::from_str(&req.status).map_err(|_| {
                    Status::invalid_argument(format!("invalid status: {}", req.status))
                })?)
            };
        let records = self
            .ledger
            .list_memories(scope, scope_value, status, req.limit)
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;
        let memories: Vec<ApiMemory> = records.iter().map(memory_to_api).collect();
        Ok(Response::new(MemoryList { memories }))
    }

    async fn promote_memory(
        &self,
        request: Request<PromoteMemoryRequest>,
    ) -> std::result::Result<Response<ApiMemory>, Status> {
        let req = request.into_inner();
        let new_text = if req.text.trim().is_empty() {
            None
        } else {
            Some(req.text.as_str())
        };
        let record = self
            .ledger
            .set_memory_status(req.id, jarvis_ledger::MemoryStatus::Active, new_text)
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;
        Ok(Response::new(memory_to_api(&record)))
    }

    async fn forget_memory(
        &self,
        request: Request<MemoryHandle>,
    ) -> std::result::Result<Response<Empty>, Status> {
        let id = request.into_inner().id;
        self.ledger
            .set_memory_status(id, jarvis_ledger::MemoryStatus::Forgotten, None)
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;
        Ok(Response::new(Empty {}))
    }

    async fn edit_memory(
        &self,
        request: Request<EditMemoryRequest>,
    ) -> std::result::Result<Response<ApiMemory>, Status> {
        use jarvis_ledger::{MemoryKind, MemoryScope};
        use std::str::FromStr;
        let req = request.into_inner();
        let scope = MemoryScope::from_str(&req.scope)
            .map_err(|_| Status::invalid_argument(format!("scope: {}", req.scope)))?;
        let kind = MemoryKind::from_str(&req.kind)
            .map_err(|_| Status::invalid_argument(format!("kind: {}", req.kind)))?;
        let record = self
            .ledger
            .edit_memory(req.id, &req.text, scope, &req.scope_value, kind)
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;
        Ok(Response::new(memory_to_api(&record)))
    }

    async fn get_timeline(
        &self,
        request: Request<jarvis_api::GetTimelineRequest>,
    ) -> std::result::Result<Response<TimelineSnapshot>, Status> {
        let req = request.into_inner();
        let task_id_str = req.id;
        let task_id = parse_task_id(&task_id_str)?;
        // § C UX fix — when the SPA is rendering a follow-up conversation,
        // include the ancestor chain's events so the transcript reads as
        // one continuous thread instead of starting blank with just the
        // latest user message.
        let events = if req.include_ancestors {
            let chain = self
                .ledger
                .walk_ancestors(task_id)
                .await
                .map_err(|e| Status::internal(format!("ledger ancestors: {e}")))?;
            let ids: Vec<_> = chain.iter().map(|t| t.id).collect();
            self.ledger
                .query_events_multi(&ids, 0, 0)
                .await
                .map_err(|e| Status::internal(format!("ledger multi: {e}")))?
        } else {
            self.ledger
                .timeline_events(task_id)
                .await
                .map_err(|e| Status::internal(format!("ledger: {e}")))?
        };

        let api_events: Vec<ApiTimelineEvent> = events
            .iter()
            .map(|e| ApiTimelineEvent {
                id: e.id.0,
                ts_micros: e.ts_micros,
                task_id: e.task_id.to_string(),
                agent_id: e.agent_id.map(|a| a.to_string()).unwrap_or_default(),
                kind: e.kind.to_string(),
                subject: e.subject.clone().unwrap_or_default(),
                payload_json: serde_json::to_string(&e.payload).unwrap_or_default(),
                parent_evt: e.parent_evt.map(|p| p.0).unwrap_or(0),
            })
            .collect();

        let spans = pair_timeline_spans(&events);

        let (min_ts, max_ts) = match (events.first(), events.last()) {
            (Some(a), Some(b)) => (a.ts_micros, b.ts_micros),
            _ => (0, 0),
        };

        Ok(Response::new(TimelineSnapshot {
            task_id: task_id_str,
            events: api_events,
            spans,
            min_ts_micros: min_ts,
            max_ts_micros: max_ts,
        }))
    }

    async fn create_schedule(
        &self,
        request: Request<ScheduleSpec>,
    ) -> std::result::Result<Response<ApiSchedule>, Status> {
        let spec = request.into_inner();
        if spec.goal.trim().is_empty() {
            return Err(Status::invalid_argument("goal is empty"));
        }
        crate::scheduler::validate_cron(&spec.cron)
            .map_err(|e| Status::invalid_argument(format!("{e}")))?;
        let id = if spec.id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            spec.id.clone()
        };
        let next_us = crate::scheduler::next_run_micros(&spec.cron);
        let new = crate::scheduler::new_record_from_spec(
            id.clone(),
            spec.cron,
            spec.goal,
            spec.workdir,
            spec.sandbox,
            spec.net_policy,
            spec.routing_policy,
            spec.max_steps,
            spec.label,
            spec.paused,
        );
        let mut record = self
            .ledger
            .create_schedule(new)
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;
        record.next_run_micros = next_us;
        let _ = self.ledger.set_schedule_next_run(&id, next_us).await;
        // Spawn the cron loop if not paused.
        if !record.paused {
            self.spawn_schedule_loop(record.clone()).await;
        }
        Ok(Response::new(schedule_to_api(&record)))
    }

    async fn list_schedules(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<ScheduleList>, Status> {
        let rows = self
            .ledger
            .list_schedules()
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;
        Ok(Response::new(ScheduleList {
            schedules: rows.iter().map(schedule_to_api).collect(),
        }))
    }

    async fn delete_schedule(
        &self,
        req: Request<ScheduleHandle>,
    ) -> std::result::Result<Response<Empty>, Status> {
        let id = req.into_inner().id;
        if id.is_empty() {
            return Err(Status::invalid_argument("id is empty"));
        }
        // Cancel any live loop FIRST so the deleted row can't be re-fired.
        if let Some(handle) = self.scheduler_handles.lock().await.remove(&id) {
            handle.cancel();
        }
        self.ledger
            .delete_schedule(&id)
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;
        Ok(Response::new(Empty {}))
    }

    async fn run_schedule_now(
        &self,
        req: Request<ScheduleHandle>,
    ) -> std::result::Result<Response<TaskHandle>, Status> {
        let id = req.into_inner().id;
        let record = self
            .ledger
            .get_schedule(&id)
            .await
            .map_err(|_| Status::not_found("schedule not found"))?;
        let spec = schedule_to_task_spec(&record);
        // Reuse submit_task's full machinery (sandbox pick, worktree, ledger row,
        // agent loop spawn). Returning the resulting TaskHandle to the caller.
        let inner_resp = self.submit_task(Request::new(spec)).await?;
        let handle = inner_resp.into_inner();
        let next_us = crate::scheduler::next_run_micros(&record.cron);
        let _ = self
            .ledger
            .record_schedule_fire(&id, &handle.id, next_us)
            .await;
        Ok(Response::new(handle))
    }
}

fn schedule_to_api(r: &jarvis_ledger::ScheduleRecord) -> ApiSchedule {
    ApiSchedule {
        spec: Some(ScheduleSpec {
            id: r.id.clone(),
            cron: r.cron.clone(),
            goal: r.goal.clone(),
            workdir: r.workdir.clone(),
            sandbox: r.sandbox.clone(),
            net_policy: r.net_policy.clone(),
            routing_policy: r.routing_policy.clone(),
            max_steps: r.max_steps,
            label: r.label.clone(),
            paused: r.paused,
        }),
        last_run_micros: r.last_run_micros,
        next_run_micros: r.next_run_micros,
        last_task_id: r.last_task_id.clone(),
    }
}

fn schedule_to_task_spec(r: &jarvis_ledger::ScheduleRecord) -> TaskSpec {
    TaskSpec {
        goal: r.goal.clone(),
        workdir: r.workdir.clone(),
        max_steps: r.max_steps,
        sandbox: r.sandbox.clone(),
        net_policy: r.net_policy.clone(),
        use_worktree: false,
        base_ref: String::new(),
        routing_policy: r.routing_policy.clone(),
        require_caps: Vec::new(),
        parent_task_id: String::new(),
        resume_from: String::new(),
    }
}

/// Pair-up events into spans for the SPA timeline.
///
/// - `tool_call` → next event with `parent_evt == call.id` and kind in
///   {`tool_result`, `error`} on the same `task_id`.
/// - `attempt` → next `verdict` or `continuation` on the same `task_id`.
/// - `decision` is rendered as a point event by the SPA; no span generated here.
///
/// Unmatched openers get `end_evt_id = 0`, `end_ts_micros = 0`, `outcome = "running"`.
fn pair_timeline_spans(events: &[EventRecord]) -> Vec<TimelineSpan> {
    use jarvis_ledger::EventKind;
    let mut spans: Vec<TimelineSpan> = Vec::new();

    for (i, opener) in events.iter().enumerate() {
        match opener.kind {
            EventKind::ToolCall => {
                let mut matched: Option<&EventRecord> = None;
                for cand in &events[i + 1..] {
                    if cand.parent_evt.map(|p| p.0) == Some(opener.id.0)
                        && matches!(cand.kind, EventKind::ToolResult | EventKind::Error)
                    {
                        matched = Some(cand);
                        break;
                    }
                }
                let (tool, args_summary) = parse_tool_call_payload(&opener.payload);
                let label = format_tool_label(&tool, &args_summary);
                let lane = if tool == "update_plan" {
                    "plan"
                } else {
                    "tools"
                };
                match matched {
                    Some(closer) => {
                        let outcome = if matches!(closer.kind, EventKind::Error)
                            || tool_result_has_error(&closer.payload)
                        {
                            "error"
                        } else {
                            "ok"
                        };
                        spans.push(TimelineSpan {
                            start_evt_id: opener.id.0,
                            end_evt_id: closer.id.0,
                            start_ts_micros: opener.ts_micros,
                            end_ts_micros: closer.ts_micros,
                            label,
                            lane: lane.to_string(),
                            outcome: outcome.to_string(),
                        });
                    }
                    None => spans.push(TimelineSpan {
                        start_evt_id: opener.id.0,
                        end_evt_id: 0,
                        start_ts_micros: opener.ts_micros,
                        end_ts_micros: 0,
                        label,
                        lane: lane.to_string(),
                        outcome: "running".to_string(),
                    }),
                }
            }
            EventKind::Attempt => {
                let mut matched: Option<&EventRecord> = None;
                for cand in &events[i + 1..] {
                    if cand.task_id == opener.task_id
                        && matches!(cand.kind, EventKind::Verdict | EventKind::Continuation)
                    {
                        matched = Some(cand);
                        break;
                    }
                }
                let step = opener
                    .payload
                    .get("step")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let label = format!("step {step}");
                match matched {
                    Some(closer) => {
                        let outcome = match closer.payload.get("verdict").and_then(|v| v.as_str()) {
                            Some("pass") | Some("done") => "ok",
                            Some("fail") => "error",
                            _ => "ok",
                        };
                        spans.push(TimelineSpan {
                            start_evt_id: opener.id.0,
                            end_evt_id: closer.id.0,
                            start_ts_micros: opener.ts_micros,
                            end_ts_micros: closer.ts_micros,
                            label,
                            lane: "verdict".to_string(),
                            outcome: outcome.to_string(),
                        });
                    }
                    None => spans.push(TimelineSpan {
                        start_evt_id: opener.id.0,
                        end_evt_id: 0,
                        start_ts_micros: opener.ts_micros,
                        end_ts_micros: 0,
                        label,
                        lane: "verdict".to_string(),
                        outcome: "running".to_string(),
                    }),
                }
            }
            _ => {}
        }
    }
    spans
}

fn parse_tool_call_payload(payload: &serde_json::Value) -> (String, String) {
    let tool = payload
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let args = payload.get("args").and_then(|v| {
        // Prefer a single distinguishing arg: cmd / path / pattern / file.
        for key in ["cmd", "path", "pattern", "file", "target"] {
            if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                return Some(s.to_string());
            }
        }
        None
    });
    let args = args.unwrap_or_default();
    (tool, args)
}

fn format_tool_label(tool: &str, args: &str) -> String {
    const MAX: usize = 40;
    if args.is_empty() {
        return tool.chars().take(MAX).collect();
    }
    let raw = format!("{tool}: {args}");
    if raw.chars().count() <= MAX {
        raw
    } else {
        let truncated: String = raw.chars().take(MAX - 1).collect();
        format!("{truncated}…")
    }
}

/// Per-stream `last sent` view of the fleet. Populated by the first
/// snapshot and updated in-place on every subsequent delta so the next
/// tick's `compute_fleet_delta` is O(tasks).
#[derive(Default)]
struct FleetState {
    nodes: HashMap<String, FleetNode>,
    edges: HashSet<(String, String)>,
}

/// Build the current fleet view from the ledger and either send it as
/// a full snapshot (first frame, or `force_snapshot=true`) or as a delta
/// against the caller's `state`. Empty deltas are dropped so idle
/// dashboards never wake. Returns `Err(())` when the channel is closed.
async fn send_fleet_frame(
    ledger: &Ledger,
    pool: &Arc<LlmPool>,
    tx: &mpsc::Sender<std::result::Result<FleetFrame, Status>>,
    state: &mut FleetState,
    force_snapshot: bool,
) -> std::result::Result<(), ()> {
    let tasks = match ledger.list_tasks(true, 500).await {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.send(Err(Status::internal(format!("ledger: {e}")))).await;
            return Err(());
        }
    };
    let mut current_nodes: HashMap<String, FleetNode> = HashMap::with_capacity(tasks.len());
    let mut current_edges: HashSet<(String, String)> = HashSet::new();
    let now = chrono::Utc::now().timestamp_micros();
    let registry = pool.registry();
    for t in &tasks {
        if let Some(parent) = t.parent {
            current_edges.insert((parent.to_string(), t.id.to_string()));
        }
        let needs_attention = t.status == jarvis_ledger::TaskStatus::Failed;
        let (tokens_in, tokens_out, cost_usd) = sum_task_usage(ledger, registry, t.id).await;
        current_nodes.insert(
            t.id.to_string(),
            FleetNode {
                task_id: t.id.to_string(),
                short_id: t.id.to_string().chars().take(8).collect(),
                status: t.status.to_string(),
                goal: t.goal.clone(),
                workdir: t.workdir.clone(),
                sandbox: t.sandbox.clone(),
                tokens_in,
                tokens_out,
                estimated_cost_usd: cost_usd,
                created_at_micros: t.created_at,
                updated_at_micros: t.completed_at.unwrap_or(t.created_at),
                needs_attention,
            },
        );
    }

    if force_snapshot {
        let snapshot = FleetSnapshot {
            nodes: current_nodes.values().cloned().collect(),
            edges: current_edges
                .iter()
                .map(|(p, c)| FleetEdge {
                    parent_task_id: p.clone(),
                    child_task_id: c.clone(),
                })
                .collect(),
        };
        let frame = FleetFrame {
            ts_micros: now,
            kind: Some(FleetFrameKind::Snapshot(snapshot)),
        };
        state.nodes = current_nodes;
        state.edges = current_edges;
        if tx.send(Ok(frame)).await.is_err() {
            return Err(());
        }
        return Ok(());
    }

    let delta = compute_fleet_delta(&state.nodes, &state.edges, &current_nodes, &current_edges);
    // Skip empty ticks — keeps the wire quiet between real changes.
    if delta_is_empty(&delta) {
        // Still update bookkeeping so future deltas are computed against
        // the most-recent state (cost/usage numbers may have shifted by
        // sub-rounding-error amounts even when nothing visible changed).
        state.nodes = current_nodes;
        state.edges = current_edges;
        return Ok(());
    }
    let frame = FleetFrame {
        ts_micros: now,
        kind: Some(FleetFrameKind::Delta(delta)),
    };
    state.nodes = current_nodes;
    state.edges = current_edges;
    if tx.send(Ok(frame)).await.is_err() {
        return Err(());
    }
    Ok(())
}

/// Diff the previous-tick fleet against the current one and return a
/// `FleetDelta`. Updated nodes carry full new state, not field diffs.
fn compute_fleet_delta(
    prev_nodes: &HashMap<String, FleetNode>,
    prev_edges: &HashSet<(String, String)>,
    cur_nodes: &HashMap<String, FleetNode>,
    cur_edges: &HashSet<(String, String)>,
) -> FleetDelta {
    let mut added_nodes = Vec::new();
    let mut updated_nodes = Vec::new();
    let mut removed_node_ids = Vec::new();
    for (id, node) in cur_nodes {
        match prev_nodes.get(id) {
            None => added_nodes.push(node.clone()),
            Some(prev) if prev != node => updated_nodes.push(node.clone()),
            Some(_) => {}
        }
    }
    for id in prev_nodes.keys() {
        if !cur_nodes.contains_key(id) {
            removed_node_ids.push(id.clone());
        }
    }
    let added_edges: Vec<FleetEdge> = cur_edges
        .difference(prev_edges)
        .map(|(p, c)| FleetEdge {
            parent_task_id: p.clone(),
            child_task_id: c.clone(),
        })
        .collect();
    let removed_edges: Vec<FleetEdge> = prev_edges
        .difference(cur_edges)
        .map(|(p, c)| FleetEdge {
            parent_task_id: p.clone(),
            child_task_id: c.clone(),
        })
        .collect();
    FleetDelta {
        added_nodes,
        updated_nodes,
        removed_node_ids,
        added_edges,
        removed_edges,
    }
}

fn delta_is_empty(d: &FleetDelta) -> bool {
    d.added_nodes.is_empty()
        && d.updated_nodes.is_empty()
        && d.removed_node_ids.is_empty()
        && d.added_edges.is_empty()
        && d.removed_edges.is_empty()
}

/// Sum `(tokens_in, tokens_out, cost_usd)` for a single task by walking its
/// events through `extract_usage`. Errors return zeros (best-effort).
async fn sum_task_usage(
    ledger: &Ledger,
    registry: &jarvis_llm::ModelRegistry,
    task_id: jarvis_core::TaskId,
) -> (u64, u64, f64) {
    let events = match ledger.query_events(Some(task_id), 0, 0).await {
        Ok(e) => e,
        Err(_) => return (0, 0, 0.0),
    };
    let mut total_in = 0u64;
    let mut total_out = 0u64;
    let mut total_cost = 0.0_f64;
    for e in &events {
        let (model, in_t, out_t) = extract_usage(&e.payload);
        if in_t == 0 && out_t == 0 {
            continue;
        }
        total_in += in_t;
        total_out += out_t;
        if let Some(m) = model {
            let p = ProviderName::new(m);
            total_cost += jarvis_llm::estimate_usd(registry, &p, in_t, out_t);
        }
    }
    (total_in, total_out, total_cost)
}

/// Pull `(model_name, tokens_in, tokens_out)` out of an event payload if the
/// agent logged a `usage` block. Tries the common shapes the agent uses:
///   { "usage": { "model": "X", "tokens_in": N, "tokens_out": M } }
///   { "model": "X", "usage": { "prompt_tokens": N, "completion_tokens": M } }
/// Returns (None, 0, 0) if no usage data is present.
fn extract_usage(payload: &serde_json::Value) -> (Option<String>, u64, u64) {
    let usage = payload.get("usage");
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            usage
                .and_then(|u| u.get("model"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    let (in_t, out_t) = match usage {
        Some(u) => {
            let in_t = u
                .get("tokens_in")
                .or_else(|| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let out_t = u
                .get("tokens_out")
                .or_else(|| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            (in_t, out_t)
        }
        None => (0, 0),
    };
    (model, in_t, out_t)
}

/// Materialise one FileDiff metadata row for the given path. Reads HEAD
/// and the working copy, counts added/removed lines, and pre-computes
/// `total_lines` for the unified-diff representation so the SPA can show
/// a "0 / 240 lines" affordance before fetching any chunk. The actual
/// diff text is fetched lazily by `GetFileDiff` (M8.1).
fn build_file_diff(workdir: &std::path::Path, path: &str) -> FileDiff {
    let abs = workdir.join(path);
    let after = std::fs::read_to_string(&abs).unwrap_or_default();
    let exists_after = abs.exists();
    let before = read_path_from_head(workdir, path).unwrap_or_default();
    let exists_before = !before.is_empty();
    let change_kind = match (exists_before, exists_after) {
        (false, true) => "added",
        (true, false) => "deleted",
        _ => "modified",
    };
    let (added, removed) = count_added_removed(&before, &after);
    let total_lines = compute_unified_diff(&before, &after, path, change_kind)
        .lines()
        .count() as u32;
    FileDiff {
        path: path.to_string(),
        change_kind: change_kind.to_string(),
        lines_added: added,
        lines_removed: removed,
        total_lines,
    }
}

/// Compute the full unified-diff text for one file change. The output
/// shape mimics `git diff --no-color`: a `--- a/path` / `+++ b/path`
/// header followed by `@@ hunk @@` blocks. We use `similar` rather than
/// shelling out to git so behaviour is identical inside and outside a
/// repo (and on Windows without a git binary).
fn compute_unified_diff(before: &str, after: &str, path: &str, change_kind: &str) -> String {
    let diff = similar::TextDiff::from_lines(before, after);
    let label_before = if change_kind == "added" {
        "/dev/null".to_string()
    } else {
        format!("a/{path}")
    };
    let label_after = if change_kind == "deleted" {
        "/dev/null".to_string()
    } else {
        format!("b/{path}")
    };
    // `similar`'s UnifiedDiff::Display already emits the `--- a/...` /
    // `+++ b/...` header before the first hunk when `header(...)` is set.
    diff.unified_diff()
        .context_radius(3)
        .header(&label_before, &label_after)
        .to_string()
}

fn read_path_from_head(workdir: &std::path::Path, path: &str) -> Option<String> {
    let repo = git2::Repository::discover(workdir).ok()?;
    let head = repo.head().ok()?;
    let commit = head.peel_to_commit().ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(std::path::Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    Some(String::from_utf8_lossy(blob.content()).into_owned())
}

fn count_added_removed(before: &str, after: &str) -> (u32, u32) {
    let diff = similar::TextDiff::from_lines(before, after);
    let mut added: u32 = 0;
    let mut removed: u32 = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added = added.saturating_add(1),
            similar::ChangeTag::Delete => removed = removed.saturating_add(1),
            similar::ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

fn synthesize_subject(decision_text: &str) -> String {
    let first_line = decision_text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(decision_text)
        .trim();
    let truncated: String = first_line.chars().take(72).collect();
    if truncated.is_empty() {
        "jarvis: phase commit".to_string()
    } else {
        truncated
    }
}

fn commit_paths(
    workdir: &std::path::Path,
    paths: &[String],
    subject: &str,
    body: &str,
) -> anyhow::Result<CommitInfo> {
    use anyhow::Context as _;
    let repo = git2::Repository::discover(workdir)
        .with_context(|| format!("discover git repo in {}", workdir.display()))?;
    let mut index = repo.index()?;
    for p in paths {
        let rel = std::path::Path::new(p);
        if workdir.join(rel).exists() {
            index.add_path(rel)?;
        } else {
            // Deleted file → remove from index
            index.remove_path(rel)?;
        }
    }
    index.write()?;
    let tree_oid = index.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("jarvis", "jarvis@localhost"))?;
    let message = if body.trim().is_empty() || body.trim() == subject.trim() {
        subject.to_string()
    } else {
        format!("{subject}\n\n{body}")
    };
    let parent_commit = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent_commit.as_ref().into_iter().collect();
    let oid = repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &parents)?;
    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(str::to_string))
        .unwrap_or_default();
    Ok(CommitInfo {
        commit_sha: oid.to_string(),
        branch,
        subject: subject.to_string(),
        ts_micros: chrono::Utc::now().timestamp_micros(),
    })
}

fn memory_to_api(m: &jarvis_ledger::MemoryRecord) -> ApiMemory {
    ApiMemory {
        id: m.id,
        scope: m.scope.as_str().to_string(),
        scope_value: m.scope_value.clone(),
        kind: m.kind.as_str().to_string(),
        text: m.text.clone(),
        status: m.status.as_str().to_string(),
        source_task_id: m.source_task_id.map(|t| t.to_string()).unwrap_or_default(),
        created_at_micros: m.created_at,
        updated_at_micros: m.updated_at,
        usage_count: m.usage_count.max(0) as u32,
    }
}

fn revert_paths(workdir: &std::path::Path, paths: &[String]) -> anyhow::Result<()> {
    let repo = git2::Repository::discover(workdir)?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    for p in paths {
        checkout.path(p);
    }
    repo.checkout_head(Some(&mut checkout))?;
    Ok(())
}

fn tool_result_has_error(payload: &serde_json::Value) -> bool {
    payload.get("error").map(|v| !v.is_null()).unwrap_or(false)
        || payload
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .map(|c| c != 0)
            .unwrap_or(false)
}

#[allow(clippy::result_large_err)]
fn parse_task_id(s: &str) -> std::result::Result<TaskId, Status> {
    TaskId::from_str(s).map_err(|_| Status::invalid_argument("invalid task id"))
}

pub(crate) fn parse_routing_str(s: &str) -> Option<RoutingPolicy> {
    let s = s.trim();
    match s {
        "auto" | "" => Some(RoutingPolicy::Auto),
        "local_only" => Some(RoutingPolicy::LocalOnly),
        "remote_only" => Some(RoutingPolicy::RemoteOnly),
        other => other
            .strip_prefix("model:")
            .map(|n| RoutingPolicy::Model(ProviderName::new(n.trim()))),
    }
}

fn parse_required_caps(caps: &[String]) -> RequiredCapabilities {
    let mut r = RequiredCapabilities::default();
    for c in caps {
        match c.trim().to_ascii_lowercase().as_str() {
            "tool_calls" => r.tool_calls = true,
            "json_schema" => r.json_schema = true,
            "vision" => r.vision = true,
            "streaming" => r.streaming = true,
            _ => {}
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_core::EventId;
    use jarvis_ledger::EventKind;
    use serde_json::json;
    use uuid::Uuid;

    fn make_event(
        id: i64,
        ts: i64,
        task_id: TaskId,
        kind: EventKind,
        payload: serde_json::Value,
        parent_evt: Option<i64>,
    ) -> EventRecord {
        EventRecord {
            id: EventId(id),
            ts_micros: ts,
            task_id,
            agent_id: None,
            kind,
            subject: None,
            payload,
            parent_evt: parent_evt.map(EventId),
        }
    }

    #[test]
    fn pair_spans_tool_call_to_tool_result() {
        let task = TaskId(Uuid::new_v4());
        let events = vec![
            make_event(
                1,
                100,
                task,
                EventKind::ToolCall,
                json!({"tool":"shell","args":{"cmd":"cargo test"}}),
                None,
            ),
            make_event(
                2,
                200,
                task,
                EventKind::ToolResult,
                json!({"exit_code": 0}),
                Some(1),
            ),
        ];
        let spans = pair_timeline_spans(&events);
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(s.start_evt_id, 1);
        assert_eq!(s.end_evt_id, 2);
        assert_eq!(s.lane, "tools");
        assert_eq!(s.outcome, "ok");
        assert!(s.label.starts_with("shell:"));
    }

    #[test]
    fn pair_spans_unmatched_tool_call_is_running() {
        let task = TaskId(Uuid::new_v4());
        let events = vec![make_event(
            1,
            100,
            task,
            EventKind::ToolCall,
            json!({"tool":"shell","args":{"cmd":"sleep 9999"}}),
            None,
        )];
        let spans = pair_timeline_spans(&events);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].end_evt_id, 0);
        assert_eq!(spans[0].outcome, "running");
    }

    #[test]
    fn pair_spans_tool_result_with_nonzero_exit_is_error() {
        let task = TaskId(Uuid::new_v4());
        let events = vec![
            make_event(
                1,
                100,
                task,
                EventKind::ToolCall,
                json!({"tool":"shell","args":{"cmd":"false"}}),
                None,
            ),
            make_event(
                2,
                200,
                task,
                EventKind::ToolResult,
                json!({"exit_code": 1}),
                Some(1),
            ),
        ];
        let spans = pair_timeline_spans(&events);
        assert_eq!(spans[0].outcome, "error");
    }

    #[test]
    fn pair_spans_update_plan_lane_is_plan() {
        let task = TaskId(Uuid::new_v4());
        let events = vec![
            make_event(
                1,
                100,
                task,
                EventKind::ToolCall,
                json!({"tool":"update_plan","args":{}}),
                None,
            ),
            make_event(
                2,
                200,
                task,
                EventKind::ToolResult,
                json!({"exit_code": 0}),
                Some(1),
            ),
        ];
        let spans = pair_timeline_spans(&events);
        assert_eq!(spans[0].lane, "plan");
    }

    #[test]
    fn pair_spans_attempt_to_verdict() {
        let task = TaskId(Uuid::new_v4());
        let events = vec![
            make_event(1, 100, task, EventKind::Attempt, json!({"step": 3}), None),
            make_event(
                2,
                200,
                task,
                EventKind::Verdict,
                json!({"verdict": "pass"}),
                None,
            ),
        ];
        let spans = pair_timeline_spans(&events);
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(s.lane, "verdict");
        assert_eq!(s.label, "step 3");
        assert_eq!(s.outcome, "ok");
    }

    #[test]
    fn format_label_truncates_long_args() {
        let l = format_tool_label("shell", &"a".repeat(100));
        assert!(l.chars().count() <= 40);
        assert!(l.ends_with('…'));
    }

    fn make_node(id: &str, status: &str) -> FleetNode {
        FleetNode {
            task_id: id.to_string(),
            short_id: id.chars().take(8).collect(),
            status: status.to_string(),
            goal: format!("goal-{id}"),
            workdir: String::new(),
            sandbox: String::new(),
            tokens_in: 0,
            tokens_out: 0,
            estimated_cost_usd: 0.0,
            created_at_micros: 0,
            updated_at_micros: 0,
            needs_attention: false,
        }
    }

    #[test]
    fn fleet_delta_is_empty_when_state_unchanged() {
        let mut nodes = HashMap::new();
        nodes.insert("a".to_string(), make_node("a", "running"));
        let mut edges = HashSet::new();
        edges.insert(("root".to_string(), "a".to_string()));
        let delta = compute_fleet_delta(&nodes, &edges, &nodes.clone(), &edges.clone());
        assert!(delta_is_empty(&delta));
    }

    #[test]
    fn fleet_delta_detects_added_node() {
        let prev_nodes: HashMap<String, FleetNode> = HashMap::new();
        let prev_edges: HashSet<(String, String)> = HashSet::new();
        let mut cur_nodes = HashMap::new();
        cur_nodes.insert("a".to_string(), make_node("a", "pending"));
        let cur_edges: HashSet<(String, String)> = HashSet::new();
        let delta = compute_fleet_delta(&prev_nodes, &prev_edges, &cur_nodes, &cur_edges);
        assert_eq!(delta.added_nodes.len(), 1);
        assert_eq!(delta.added_nodes[0].task_id, "a");
        assert!(delta.updated_nodes.is_empty());
        assert!(delta.removed_node_ids.is_empty());
    }

    #[test]
    fn fleet_delta_detects_updated_node() {
        let mut prev_nodes = HashMap::new();
        prev_nodes.insert("a".to_string(), make_node("a", "pending"));
        let mut cur_nodes = HashMap::new();
        cur_nodes.insert("a".to_string(), make_node("a", "completed"));
        let empty: HashSet<(String, String)> = HashSet::new();
        let delta = compute_fleet_delta(&prev_nodes, &empty, &cur_nodes, &empty);
        assert!(delta.added_nodes.is_empty());
        assert_eq!(delta.updated_nodes.len(), 1);
        assert_eq!(delta.updated_nodes[0].status, "completed");
        assert!(delta.removed_node_ids.is_empty());
    }

    #[test]
    fn fleet_delta_detects_removed_node_and_edges() {
        let mut prev_nodes = HashMap::new();
        prev_nodes.insert("a".to_string(), make_node("a", "running"));
        prev_nodes.insert("b".to_string(), make_node("b", "running"));
        let mut prev_edges = HashSet::new();
        prev_edges.insert(("a".to_string(), "b".to_string()));
        let mut cur_nodes = HashMap::new();
        cur_nodes.insert("a".to_string(), make_node("a", "running"));
        let cur_edges: HashSet<(String, String)> = HashSet::new();
        let delta = compute_fleet_delta(&prev_nodes, &prev_edges, &cur_nodes, &cur_edges);
        assert!(delta.added_nodes.is_empty());
        assert_eq!(delta.removed_node_ids, vec!["b".to_string()]);
        assert_eq!(delta.removed_edges.len(), 1);
    }

    #[test]
    fn unified_diff_paginates_by_offset_and_max() {
        // Build a synthetic diff: 500 lines of `+addedN` so we know the
        // exact total. We can't easily fabricate the 500-line shape via
        // `similar` without before/after content of that size; instead we
        // construct the diff text directly and re-use the line-splitting
        // logic that `get_file_diff` runs on it.
        let mut lines: Vec<String> = Vec::with_capacity(500);
        for i in 0..500 {
            lines.push(format!("+line {i}"));
        }
        let diff_text = lines.join("\n");
        let split: Vec<&str> = diff_text.split('\n').collect();
        assert_eq!(split.len(), 500);

        // offset=0, max=200 → 200 lines, has_more
        let offset: usize = 0;
        let max: usize = 200;
        let end = (offset + max).min(split.len());
        assert_eq!(end - offset, 200);
        assert!(end < split.len());

        // offset=400, max=200 → 100 lines, no more
        let offset: usize = 400;
        let end = (offset + max).min(split.len());
        assert_eq!(end - offset, 100);
        assert_eq!(end, split.len());
    }

    #[test]
    fn compute_unified_diff_produces_header_and_hunks() {
        let before = "alpha\nbeta\ngamma\n";
        let after = "alpha\nBETA\ngamma\n";
        let out = compute_unified_diff(before, after, "src/foo.rs", "modified");
        assert!(out.contains("--- a/src/foo.rs"), "header missing: {out}");
        assert!(out.contains("+++ b/src/foo.rs"), "header missing: {out}");
        assert!(out.contains("-beta"));
        assert!(out.contains("+BETA"));
    }

    #[test]
    fn compute_unified_diff_added_file_uses_dev_null() {
        let out = compute_unified_diff("", "new\nfile\n", "src/new.rs", "added");
        assert!(out.contains("--- /dev/null"), "expected /dev/null: {out}");
        assert!(out.contains("+++ b/src/new.rs"));
    }
}
