//! gRPC service implementation. M1: Ping, Ask. M2: SubmitTask, GetTask, ListTasks,
//! CancelTask, StreamEvents. M3: sandbox + worktree per task.

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use jarvis_agent::{run_agent, AgentRun};
use jarvis_api::{
    jarvis_server::{Jarvis, JarvisServer},
    AskChunk, AskRequest, Event as ApiEvent, ListTasksRequest, PingRequest, PingResponse,
    StreamEventsRequest, Task as ApiTask, TaskHandle, TaskList, TaskSpec, UsageStats,
};
use jarvis_config::{Config, LocalProvider};
use jarvis_core::{AgentId, Capabilities, ChatMessage, ChatRequest, LlmProvider, ProviderName, TaskId};
use jarvis_ledger::{EventRecord, Ledger, TaskRecord, TaskRuntimeInfo};
use jarvis_llm::{OpenAiCompatConfig, OpenAiCompatProvider};
use jarvis_sandbox::{
    DockerSandbox, NativeSandbox, NetPolicy, Sandbox, SandboxKind, Worktree, WorktreeManager,
};
use jarvis_tools::{FsReadTool, FsWriteTool, ShellTool, ToolCtx, ToolRegistry};
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
    let provider = build_default_provider(&cfg)
        .context("no usable LLM provider in config")?;

    let ledger_path = cfg.daemon.data_dir.join("ledger.sqlite");
    let ledger = Ledger::open(&ledger_path).await.context("open ledger")?;

    let tools = build_tool_registry();

    // Boot the sandbox factory. Native is always available; Docker is optional.
    let native: Arc<dyn Sandbox> = Arc::new(NativeSandbox);
    let docker = build_docker_sandbox(&cfg.sandbox).await;
    if docker.is_none() && cfg.sandbox.default_backend == "docker" {
        warn!("config requests docker backend but daemon could not connect — tasks will fall back to native");
    }

    let worktrees = WorktreeManager::new(cfg.daemon.data_dir.join("worktrees"));

    let svc = JarvisService::new(provider, ledger, tools, native, docker, worktrees, cfg);

    let addr: std::net::SocketAddr = bind.parse().context("parse daemon.addr")?;
    info!(%addr, ledger = %ledger_path.display(), "jarvis-daemon listening");

    Server::builder()
        .add_service(JarvisServer::new(svc))
        .serve(addr)
        .await
        .context("gRPC server")?;
    Ok(())
}

fn build_default_provider(cfg: &Config) -> Result<Arc<dyn LlmProvider>> {
    let (name, lp) = cfg
        .providers
        .local
        .iter()
        .next()
        .ok_or_else(|| anyhow!("no [providers.local.*] entry in config"))?;
    let oc = build_local_provider_cfg(name, lp);
    info!(provider = %oc.name, model = %oc.model, base_url = %oc.base_url, "boot provider");
    Ok(Arc::new(OpenAiCompatProvider::new(oc)))
}

fn build_local_provider_cfg(name: &str, lp: &LocalProvider) -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        name: ProviderName::new(format!("local:{name}")),
        base_url: lp.url.clone(),
        model: lp.model.clone(),
        api_key: lp.api_key.clone().unwrap_or_default(),
        capabilities: Capabilities {
            ctx_len: lp.capabilities.ctx_len,
            tool_calls: lp.capabilities.tool_calls,
            json_schema: lp.capabilities.json_schema,
            vision: lp.capabilities.vision,
            supports_streaming: lp.capabilities.supports_streaming,
        },
    }
}

fn build_tool_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(FsReadTool);
    r.register(FsWriteTool);
    r.register(ShellTool);
    r
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

struct JarvisService {
    provider: Arc<dyn LlmProvider>,
    ledger: Ledger,
    tools: ToolRegistry,
    native: Arc<dyn Sandbox>,
    docker: Option<Arc<dyn Sandbox>>,
    worktrees: Arc<WorktreeManager>,
    cfg: Config,
    started: Instant,
    running: Arc<Mutex<HashMap<TaskId, RuntimeHandle>>>,
}

struct RuntimeHandle {
    cancel: CancellationToken,
    source_workdir: PathBuf,
    worktree: Worktree,
}

impl JarvisService {
    fn new(
        provider: Arc<dyn LlmProvider>,
        ledger: Ledger,
        tools: ToolRegistry,
        native: Arc<dyn Sandbox>,
        docker: Option<Arc<dyn Sandbox>>,
        worktrees: WorktreeManager,
        cfg: Config,
    ) -> Self {
        Self {
            provider,
            ledger,
            tools,
            native,
            docker,
            worktrees: Arc::new(worktrees),
            cfg,
            started: Instant::now(),
            running: Arc::new(Mutex::new(HashMap::new())),
        }
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
            .provider
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
        let source_workdir = if spec.workdir.is_empty() {
            std::env::current_dir().unwrap_or(PathBuf::from("."))
        } else {
            PathBuf::from(&spec.workdir)
        };

        // Pick sandbox + net policy (fail fast on bad inputs).
        let (sandbox, kind) = self.pick_sandbox(&spec.sandbox)?;
        let net = self.pick_net(&spec.net_policy)?;

        // Create the task row before the worktree so the worktree can use the task id.
        let task = self
            .ledger
            .create_task(&spec.goal, &source_workdir.display().to_string(), None)
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
        let run = AgentRun {
            task_id: task.id,
            workdir: worktree.path.clone(),
            max_steps: if spec.max_steps == 0 { 20 } else { spec.max_steps },
            agent_id: AgentId::new(),
            cancel,
        };

        let provider = self.provider.clone();
        let ledger = self.ledger.clone();
        let tools = self.tools.clone();
        let running = self.running.clone();
        let worktrees = self.worktrees.clone();
        let task_id = task.id;
        tokio::spawn(async move {
            let outcome = run_agent(run, provider, ledger, tools, ctx).await;
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

        let backfill = self
            .ledger
            .query_events(task_filter, req.since_id, 0)
            .await
            .map_err(|e| Status::internal(format!("ledger: {e}")))?;
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
}

#[allow(clippy::result_large_err)]
fn parse_task_id(s: &str) -> std::result::Result<TaskId, Status> {
    TaskId::from_str(s).map_err(|_| Status::invalid_argument("invalid task id"))
}
