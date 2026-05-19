use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use futures::StreamExt;
use jarvis_api::{
    AskRequest, ListTasksRequest, PingRequest, StatusRequest, StreamEventsRequest, TaskHandle,
    TaskSpec,
    auth::{ClientAuth, discover_token},
    jarvis_client::JarvisClient,
};
use std::io::{self, Write};
use std::time::Duration;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};

/// Type of the authenticated client returned by `build_client`. The closure
/// type from `with_interceptor` is unnameable, so we use the concrete
/// `ClientAuth` interceptor and a type alias for ergonomics.
type AuthedClient = JarvisClient<InterceptedService<Channel, ClientAuth>>;

#[derive(Debug, Parser)]
#[command(name = "jarvis", version, about = "Jarvis CLI client")]
struct Cli {
    /// Daemon endpoint. `http://host:port` (UDS/named-pipe TBD).
    #[arg(
        long,
        env = "JARVIS_DAEMON_URL",
        default_value = "http://127.0.0.1:7777"
    )]
    daemon: String,

    /// Connect timeout in seconds.
    #[arg(long, default_value_t = 3)]
    connect_timeout: u64,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Health-check the daemon.
    Ping,
    /// One-shot LLM ask, streamed to stdout (bypasses the agent loop).
    Ask {
        prompt: Vec<String>,
        #[arg(long)]
        temperature: Option<f32>,
        #[arg(long)]
        max_tokens: Option<u32>,
    },
    /// Show daemon-wide status (models, uptime, running tasks).
    Status,
    /// Manage agent tasks.
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum TaskCmd {
    /// Submit a new task to the daemon.
    Add(AddTaskArgs),
    /// Watch the live event stream for a task (or for ALL tasks).
    Watch {
        /// Task id (UUID). Omit to watch all tasks.
        id: Option<String>,
        /// Don't replay history, only stream new events.
        #[arg(long)]
        tail_only: bool,
        /// Stop after backfill instead of following.
        #[arg(long)]
        no_follow: bool,
    },
    /// List tasks.
    List {
        /// Include completed/failed/cancelled tasks.
        #[arg(long)]
        all: bool,
        /// Max entries.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Show one task.
    Get { id: String },
    /// Cancel a running task.
    Cancel { id: String },
}

#[derive(Debug, Args)]
struct AddTaskArgs {
    /// The goal description. Multiple args are joined with spaces.
    goal: Vec<String>,
    /// Working directory the agent operates on (defaults to cwd).
    #[arg(long)]
    workdir: Option<String>,
    /// Max agent steps before aborting.
    #[arg(long, default_value_t = 20)]
    max_steps: u32,
    /// Sandbox backend: native | docker. Defaults to config.
    #[arg(long)]
    sandbox: Option<String>,
    /// Network policy (docker only): none | egress_only | full. Defaults to config.
    #[arg(long = "net")]
    net_policy: Option<String>,
    /// Create a fresh git worktree for the task (if workdir is in a git repo).
    #[arg(long)]
    worktree: bool,
    /// Base git ref to branch the worktree from (default: HEAD).
    #[arg(long)]
    base_ref: Option<String>,
    /// Routing policy: auto | local_only | remote_only | model:&lt;name&gt;.
    #[arg(long = "routing")]
    routing: Option<String>,
    /// Strict force a model by name (shorthand for --routing model:&lt;name&gt;).
    #[arg(long)]
    model: Option<String>,
    /// Required capabilities (repeatable): tool_calls, json_schema, vision.
    #[arg(long = "require")]
    require: Vec<String>,
    /// Continue a conversation: parent task id (UUID). Inherits workdir / sandbox.
    #[arg(long = "parent")]
    parent: Option<String>,
    /// Resume an interrupted task: source task id (UUID). Inherits
    /// workdir/sandbox/net/worktree from the source and logs a continuation
    /// event referencing it. The agent loop picks up with the full history
    /// of the source in its prompt context.
    #[arg(long = "resume")]
    resume: Option<String>,
    /// Watch events live after submission.
    #[arg(long)]
    watch: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let channel = connect(&cli.daemon, cli.connect_timeout).await?;
    let mut client = build_client(channel)?;

    match cli.cmd {
        Cmd::Ping => {
            let r = client.ping(PingRequest {}).await?.into_inner();
            println!(
                "ok — jarvis-daemon {} (uptime {}s)",
                r.version, r.uptime_seconds
            );
        }
        Cmd::Ask {
            prompt,
            temperature,
            max_tokens,
        } => {
            if prompt.is_empty() {
                anyhow::bail!("prompt is empty; pass it as positional args");
            }
            let req = AskRequest {
                prompt: prompt.join(" "),
                provider: String::new(),
                temperature,
                max_tokens,
            };
            let mut stream = client.ask(req).await?.into_inner();
            let mut stdout = io::stdout().lock();
            let mut last_usage = None;
            while let Some(item) = stream.next().await {
                let chunk = item.context("stream error")?;
                if !chunk.delta.is_empty() {
                    stdout.write_all(chunk.delta.as_bytes())?;
                    stdout.flush()?;
                }
                if chunk.usage.is_some() {
                    last_usage = chunk.usage;
                }
            }
            writeln!(stdout)?;
            if let Some(u) = last_usage {
                eprintln!(
                    "\n— tokens: prompt={}, completion={}, total={}",
                    u.prompt_tokens, u.completion_tokens, u.total_tokens
                );
            }
        }
        Cmd::Status => {
            let s = client.get_status(StatusRequest {}).await?.into_inner();
            println!(
                "daemon: v{}  ·  up {}s  ·  {} running tasks",
                s.version, s.uptime_seconds, s.running_tasks
            );
            println!();
            println!(
                "  {:<26} {:<7} {:<3} {:<7} model_id",
                "model", "kind", "pri", "status"
            );
            for m in s.models {
                let status = if !m.online {
                    "offline"
                } else if m.quarantined {
                    "quar"
                } else {
                    "ok"
                };
                println!(
                    "  {:<26} {:<7} {:<3} {:<7} {}",
                    m.name, m.kind, m.priority, status, m.model_id
                );
            }
        }
        Cmd::Task { cmd } => task_cmd(&mut client, cmd).await?,
    }
    Ok(())
}

async fn task_cmd(client: &mut AuthedClient, cmd: TaskCmd) -> Result<()> {
    match cmd {
        TaskCmd::Add(a) => {
            if a.goal.is_empty() {
                anyhow::bail!("goal is empty");
            }
            let workdir = a.workdir.unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            });
            // --model is shorthand for --routing model:<name>; --routing wins if both given.
            let routing_policy = a
                .routing
                .clone()
                .or_else(|| a.model.as_ref().map(|m| format!("model:{m}")))
                .unwrap_or_default();
            let spec = TaskSpec {
                goal: a.goal.join(" "),
                workdir,
                max_steps: a.max_steps,
                sandbox: a.sandbox.unwrap_or_default(),
                net_policy: a.net_policy.unwrap_or_default(),
                use_worktree: a.worktree,
                base_ref: a.base_ref.unwrap_or_default(),
                routing_policy,
                require_caps: a.require,
                parent_task_id: a.parent.unwrap_or_default(),
                resume_from: a.resume.clone().unwrap_or_default(),
            };
            let h = client.submit_task(spec).await?.into_inner();
            println!("submitted task: {}", h.id);
            if a.watch {
                watch_task(client, Some(h.id), false, false).await?;
            }
        }
        TaskCmd::Watch {
            id,
            tail_only,
            no_follow,
        } => watch_task(client, id, tail_only, no_follow).await?,
        TaskCmd::List { all, limit } => {
            let resp = client
                .list_tasks(ListTasksRequest {
                    include_finished: all,
                    limit,
                })
                .await?
                .into_inner();
            if resp.tasks.is_empty() {
                println!("(no tasks)");
            }
            for t in resp.tasks {
                let backend = if t.sandbox.is_empty() {
                    String::from("-")
                } else if t.net_policy.is_empty() || t.sandbox == "native" {
                    t.sandbox.clone()
                } else {
                    format!("{}/{}", t.sandbox, t.net_policy)
                };
                println!(
                    "{}  [{:<9}] {:<14} {}",
                    short_id(&t.id),
                    t.status,
                    backend,
                    truncate(&t.goal, 80)
                );
            }
        }
        TaskCmd::Get { id } => {
            let t = client.get_task(TaskHandle { id }).await?.into_inner();
            println!("id:              {}", t.id);
            println!("status:          {}", t.status);
            println!("goal:            {}", t.goal);
            println!("workdir:         {}", t.workdir);
            println!(
                "sandbox:         {}",
                if t.sandbox.is_empty() {
                    "-"
                } else {
                    &t.sandbox
                }
            );
            println!(
                "net_policy:      {}",
                if t.net_policy.is_empty() {
                    "-"
                } else {
                    &t.net_policy
                }
            );
            if !t.worktree_path.is_empty() {
                println!("worktree_path:   {}", t.worktree_path);
                println!("worktree_branch: {}", t.worktree_branch);
            }
            println!("created_at:      {} (micros)", t.created_at);
            if let Some(c) = t.completed_at {
                println!("completed_at:    {} (micros)", c);
            }
            if let Some(e) = t.error {
                println!("error:           {}", e);
            }
        }
        TaskCmd::Cancel { id } => {
            client.cancel_task(TaskHandle { id: id.clone() }).await?;
            println!("cancelled: {}", id);
        }
    }
    Ok(())
}

async fn watch_task(
    client: &mut AuthedClient,
    id: Option<String>,
    tail_only: bool,
    no_follow: bool,
) -> Result<()> {
    let req = StreamEventsRequest {
        task_id: id.unwrap_or_default(),
        follow: !no_follow,
        since_id: if tail_only { i64::MAX } else { 0 },
        include_ancestors: !tail_only,
    };
    let mut stream = client.stream_events(req).await?.into_inner();
    while let Some(item) = stream.next().await {
        let ev = item.context("stream error")?;
        print_event(&ev);
    }
    Ok(())
}

fn print_event(ev: &jarvis_api::Event) {
    let ts = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(ev.ts_micros)
        .map(|t| t.format("%H:%M:%S%.3f").to_string())
        .unwrap_or_else(|| ev.ts_micros.to_string());
    let subj = if ev.subject.is_empty() {
        String::new()
    } else {
        format!(" {}", ev.subject)
    };
    let payload = pretty_payload(&ev.kind, &ev.payload_json);
    println!(
        "{ts}  {kind:<14} {evt:>5}{subj}  {payload}",
        kind = ev.kind,
        evt = format!("#{}", ev.id),
    );
}

fn pretty_payload(kind: &str, json: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return truncate(json, 200),
    };
    let summary = match kind {
        "decision" => v.get("thought").and_then(|s| s.as_str()).map(String::from),
        "tool_call" => Some(format!(
            "{}({})",
            v.get("tool").and_then(|s| s.as_str()).unwrap_or("?"),
            truncate(
                &v.get("args").map(|a| a.to_string()).unwrap_or_default(),
                120
            )
        )),
        "tool_result" => v.get("summary").and_then(|s| s.as_str()).map(String::from),
        "error" => v
            .get("message")
            .or_else(|| v.get("error"))
            .and_then(|s| s.as_str())
            .map(String::from),
        "verdict" => v.get("message").and_then(|s| s.as_str()).map(|m| {
            let verdict = v.get("verdict").and_then(|s| s.as_str()).unwrap_or("?");
            format!("[{verdict}] {m}")
        }),
        _ => None,
    };
    summary.unwrap_or_else(|| truncate(json, 200))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut t = s[..max].to_string();
        t.push('…');
        t
    }
}

fn short_id(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_string()
}

fn build_client(channel: Channel) -> Result<AuthedClient> {
    let token = discover_token().unwrap_or_default();
    let auth =
        ClientAuth::new(&token).map_err(|e| anyhow::anyhow!("invalid token in env/file: {e}"))?;
    Ok(JarvisClient::with_interceptor(channel, auth))
}

async fn connect(endpoint: &str, timeout_secs: u64) -> Result<Channel> {
    let ep = Endpoint::from_shared(endpoint.to_string())
        .context("invalid daemon endpoint")?
        .connect_timeout(Duration::from_secs(timeout_secs))
        .timeout(Duration::from_secs(3600));
    ep.connect()
        .await
        .with_context(|| format!("connect to {endpoint}"))
}
