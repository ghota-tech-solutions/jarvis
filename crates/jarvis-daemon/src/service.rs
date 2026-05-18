//! gRPC service implementation. M1: Ping, Ask. M2: SubmitTask, GetTask, ListTasks,
//! CancelTask, StreamEvents. M3: sandbox + worktree per task.

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use jarvis_agent::{run_agent, AgentRun, HookSpec};
use jarvis_api::{
    jarvis_server::{Jarvis, JarvisServer},
    AskChunk, AskRequest, DaemonStatus, Event as ApiEvent, ListTasksRequest,
    ModelStatus as ApiModelStatus, PingRequest, PingResponse, StatusRequest, StreamEventsRequest,
    Task as ApiTask, TaskHandle, TaskList, TaskSpec, UsageStats,
};
use jarvis_config::Config;
use jarvis_core::{
    AgentId, Capabilities, ChatMessage, ChatRequest, LlmProvider, ProviderName, RequiredCapabilities,
    RoutingPolicy, TaskId, TaskKind,
};
use jarvis_ledger::{EventRecord, Ledger, TaskRecord, TaskRuntimeInfo};
use jarvis_llm::{
    make_openai_compat_entry, LlmPool, ModelKind, ModelRegistry, OpenAiCompatConfig,
    OpenAiCompatProvider, QuarantineConfig,
};
use jarvis_mcp::{McpClient, McpServerSpec, McpToolAdapter};
use jarvis_sandbox::{
    DockerSandbox, NativeSandbox, NetPolicy, Sandbox, SandboxKind, Worktree, WorktreeManager,
};
use jarvis_tools::{
    ApplyPatchTool, FsReadTool, FsWriteTool, GlobTool, GrepTool, ShellTool, ToolCtx, ToolRegistry,
    UpdatePlanTool,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tokio_util::sync::CancellationToken;
use tonic::{transport::Server, Request, Response, Status};
use tracing::{error, info, instrument, warn};

pub async fn run(cfg: Config, bind: String) -> Result<()> {
    let registry = build_registry(&cfg)?;
    if registry.is_empty() {
        return Err(anyhow!("no providers configured — add at least one [providers.local.*]"));
    }
    info!(models = registry.len(), "model registry built");

    let pool = Arc::new(LlmPool::new(registry, QuarantineConfig {
        threshold: cfg.routing.quarantine_after_failures,
        window: chrono::Duration::minutes(cfg.routing.quarantine_window_minutes as i64),
        duration: chrono::Duration::minutes(cfg.routing.quarantine_duration_minutes as i64),
    }));
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
        warn!("config requests docker backend but daemon could not connect — tasks will fall back to native");
    }

    let worktrees = Arc::new(WorktreeManager::new(cfg.daemon.data_dir.join("worktrees")));

    let running: Arc<Mutex<HashMap<TaskId, RuntimeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
    let started = Instant::now();

    let svc = JarvisService {
        pool: pool.clone(),
        ask_provider,
        ledger: ledger.clone(),
        tools: tools.clone(),
        native: native.clone(),
        docker: docker.clone(),
        worktrees: worktrees.clone(),
        cfg: cfg.clone(),
        started,
        running: running.clone(),
        mcp_status: mcp_status.clone(),
    };

    let addr: std::net::SocketAddr = bind.parse().context("parse daemon.addr")?;
    info!(%addr, ledger = %ledger_path.display(), "jarvis-daemon listening");

    // Optionally start the embedded web UI alongside gRPC.
    let web_handle = if cfg.web.enable {
        let web_state = super::web::WebState {
            pool: pool.clone(),
            ledger: ledger.clone(),
            tools: tools.clone(),
            native: native.clone(),
            docker: docker.clone(),
            worktrees: worktrees.clone(),
            cfg: Arc::new(cfg.clone()),
            started,
            running: running.clone(),
            mcp_status: mcp_status.clone(),
        };
        let web_addr = cfg.web.addr.clone();
        Some(tokio::spawn(async move {
            if let Err(e) = super::web::serve(web_state, web_addr).await {
                warn!(error = %e, "web server stopped");
            }
        }))
    } else {
        None
    };

    // M6: start the new SPA layer on a separate port (default 7879).
    // It coexists with the legacy HTMX UI above and consumes the daemon
    // only via the public gRPC API — no internal type sharing.
    let spa_handle = if cfg.web.enable {
        let spa_addr = jarvis_web::resolve_addr();
        Some(tokio::spawn(async move {
            if let Err(e) = jarvis_web::serve(spa_addr).await {
                warn!(error = %e, "jarvis-web SPA server stopped");
            }
        }))
    } else {
        None
    };

    let result = Server::builder()
        .add_service(JarvisServer::new(svc))
        .serve(addr)
        .await
        .context("gRPC server");

    if let Some(h) = web_handle {
        h.abort();
    }
    if let Some(h) = spa_handle {
        h.abort();
    }
    result
}

fn build_registry(cfg: &Config) -> Result<ModelRegistry> {
    let mut r = ModelRegistry::new();
    for (name, lp) in &cfg.providers.local {
        let entry = make_openai_compat_entry(
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
        info!(model = %entry.name, model_id = %entry.model_id, "registered local model");
        r.insert(entry);
    }
    for (name, rp) in &cfg.providers.remote {
        let entry = make_openai_compat_entry(
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
        info!(model = %entry.name, model_id = %entry.model_id, "registered remote model");
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
pub struct McpServerStatus {
    pub name: String,
    pub connected: bool,
    pub tools: Vec<String>,
    pub error: Option<String>,
}

/// Compile `[hooks.post_tool]` config entries into runtime `HookSpec`s.
/// Invalid regex or empty match patterns are logged and skipped — we never
/// fail task dispatch because of a typo'd hook.
pub fn compile_hooks(cfg: &jarvis_config::HooksConfig) -> Vec<HookSpec> {
    let mut out = Vec::with_capacity(cfg.post_tool.len());
    for h in &cfg.post_tool {
        let pat = h.r#match.trim();
        if pat.is_empty() {
            warn!("hooks.post_tool: skipping entry with empty `match`");
            continue;
        }
        let re = match regex::Regex::new(pat) {
            Ok(r) => r,
            Err(e) => {
                warn!(pattern = pat, error = %e, "hooks.post_tool: invalid regex — skipping");
                continue;
            }
        };
        let label = h
            .label
            .clone()
            .or_else(|| h.cmd.split_whitespace().next().map(|s| s.to_string()))
            .unwrap_or_else(|| "hook".to_string());
        out.push(HookSpec {
            label,
            matcher: re,
            cmd: h.cmd.clone(),
            workdir: h.workdir.clone(),
            timeout: std::time::Duration::from_secs(h.timeout_s),
        });
    }
    out
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
    pub worktrees: Arc<WorktreeManager>,
    pub cfg: Config,
    pub started: Instant,
    pub running: Arc<Mutex<HashMap<TaskId, RuntimeHandle>>>,
    #[allow(dead_code)]
    pub mcp_status: Arc<Vec<McpServerStatus>>,
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
            return Ok(parse_routing_str(&self.cfg.routing.default_policy)
                .unwrap_or(RoutingPolicy::Auto));
        }
        parse_routing_str(raw).ok_or_else(|| Status::invalid_argument(format!("invalid routing: {raw}")))
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
    async fn ping(&self, _req: Request<PingRequest>) -> std::result::Result<Response<PingResponse>, Status> {
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
        let spec = req.into_inner();
        if spec.goal.trim().is_empty() {
            return Err(Status::invalid_argument("goal is empty"));
        }
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
            parent_record.as_ref().map(|p| p.sandbox.clone()).unwrap_or_default()
        };
        let net_pref = if !spec.net_policy.is_empty() {
            spec.net_policy.clone()
        } else {
            parent_record.as_ref().map(|p| p.net_policy.clone()).unwrap_or_default()
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
            match tokio::task::spawn_blocking(move || {
                mgr.create(task_id, &source, base.as_deref())
            })
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
        };
        // Routing policy / required caps for this run.
        let routing = self.parse_routing(&spec.routing_policy)?;
        let required = parse_required_caps(&spec.require_caps);

        let sandbox_mode = self
            .cfg
            .sandbox
            .default_mode
            .parse()
            .unwrap_or_default();
        let run = AgentRun {
            task_id: task.id,
            workdir: worktree.path.clone(),
            max_steps: if spec.max_steps == 0 { 20 } else { spec.max_steps },
            agent_id: AgentId::new(),
            cancel,
            routing,
            required,
            kind: TaskKind::Planning,
            continuation_budget: 1,
            hooks: compile_hooks(&self.cfg.hooks),
            sandbox_mode,
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

        Ok(Response::new(TaskHandle { id: task.id.to_string() }))
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
        let maybe = self
            .running
            .lock()
            .await
            .get(&id)
            .map(|h| h.cancel.clone());
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
        Ok(Response::new(DaemonStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_seconds: self.started.elapsed().as_secs() as i64,
            models: api_models,
            running_tasks: running,
        }))
    }
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
