//! jarvis-bench CLI — `jarvis-bench run --suite <path> [...]`.

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use clap::{Parser, Subcommand};
use jarvis_bench::{
    ConfigSummary, RunOptions, Scorecard, StubProvider, Suite, TaskOutcome, run_task,
};
use jarvis_config::Config;
use jarvis_core::{Capabilities, LlmProvider, RoutingPolicy};
use jarvis_llm::{
    LlmPool, ModelEntry, ModelKind, ModelRegistry, QuarantineConfig, make_openai_compat_entry,
};
use jarvis_tools::{
    ApplyPatchTool, FsReadTool, FsWriteTool, GlobTool, GrepTool, ShellTool, ToolRegistry,
    UpdatePlanTool,
};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "jarvis-bench",
    version,
    about = "Reproducible benchmark harness for Jarvis"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a YAML suite and emit a JSON scorecard.
    Run(RunArgs),
}

#[derive(Parser, Debug)]
struct RunArgs {
    /// Path to the suite YAML file.
    #[arg(long)]
    suite: PathBuf,

    /// Optional jarvis.toml. Defaults to `JarvisConfig::default()` if absent.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Routing policy: auto | local_only | remote_only.
    #[arg(long, default_value = "auto")]
    routing: String,

    /// Output scorecard JSON path.
    #[arg(long)]
    output: PathBuf,

    /// Run only this task id (filter to a single entry in the suite).
    #[arg(long)]
    task: Option<String>,

    /// Don't clean up the per-task tempdir. Useful for inspecting state
    /// after a failure.
    #[arg(long)]
    keep_tempdir: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("JARVIS_BENCH_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run(args) => cmd_run(args).await,
    }
}

async fn cmd_run(args: RunArgs) -> Result<()> {
    let suite = Suite::from_path(&args.suite)
        .with_context(|| format!("loading suite `{}`", args.suite.display()))?;

    // Filter to a single task if --task was provided.
    let mut tasks = suite.tasks.clone();
    if let Some(id) = &args.task {
        tasks.retain(|t| &t.id == id);
        if tasks.is_empty() {
            return Err(anyhow!("no task with id `{id}` in suite `{}`", suite.suite));
        }
    }

    // Load config (best-effort) and build the registry.
    let cfg = load_config(args.config.as_deref())?;
    let (registry, summary_model) = build_registry(&cfg);
    let pool = Arc::new(LlmPool::new(registry, QuarantineConfig::default()));

    let routing = parse_routing(&args.routing)?;
    let opts = RunOptions {
        routing: routing.clone(),
        sandbox_mode: cfg.sandbox.default_mode.parse().unwrap_or_default(),
        keep_tempdir: args.keep_tempdir,
    };

    let config_summary = ConfigSummary {
        model: summary_model,
        routing: args.routing.clone(),
        sandbox: "native".to_string(), // bench always runs Native for portability
    };

    let started_at = Utc::now();
    info!(
        suite = %suite.suite,
        tasks = tasks.len(),
        model = %config_summary.model,
        "bench: starting"
    );

    let tools = build_tools();
    let mut results = Vec::with_capacity(tasks.len());
    for task in &tasks {
        info!(id = %task.id, goal = %task.goal, "running task");
        let result = run_task(task, &suite.defaults, &opts, pool.clone(), tools.clone()).await;
        print_task_line(&result);
        results.push(result);
    }

    let scorecard = Scorecard::new(suite.suite.clone(), started_at, config_summary, results);
    print_summary(&scorecard);

    let json = serde_json::to_string_pretty(&scorecard)?;
    if let Some(parent) = args.output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p `{}`", parent.display()))?;
    }
    std::fs::write(&args.output, &json)
        .with_context(|| format!("writing scorecard to `{}`", args.output.display()))?;
    info!(path = %args.output.display(), "scorecard written");
    Ok(())
}

fn load_config(path: Option<&std::path::Path>) -> Result<Config> {
    jarvis_config::load(path).context("loading jarvis config")
}

/// Build a `ModelRegistry` from config. If no providers are configured *and*
/// no env vars are set, fall back to the `StubProvider` so the bench can run
/// end-to-end without a real LLM (mostly to validate the harness itself).
fn build_registry(cfg: &Config) -> (ModelRegistry, String) {
    let mut registry = ModelRegistry::new();
    let mut summary = String::new();

    // M0: explicit env-var overlays, useful for `HERMES_LOCAL_*` style local runs.
    let env_url = std::env::var("HERMES_LOCAL_URL").ok();
    let env_model = std::env::var("HERMES_LOCAL_MODEL").ok();
    let env_key = std::env::var("HERMES_LOCAL_API_KEY").ok();
    if let (Some(url), Some(model)) = (env_url.as_ref(), env_model.as_ref()) {
        let entry = make_openai_compat_entry(
            "hermes",
            ModelKind::Local,
            url.clone(),
            model.clone(),
            env_key.clone().unwrap_or_default(),
            10,
            Capabilities {
                ctx_len: 32_000,
                tool_calls: true,
                json_schema: false,
                vision: false,
                supports_streaming: true,
            },
            0.0,
            0.0,
        );
        summary = entry.name.as_str().to_string();
        registry.insert(entry);
    }

    // Config-declared providers (mirrors daemon::build_registry).
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
        entry.tool_dialect = lp.tool_dialect;
        entry.thinking = lp.thinking;
        if summary.is_empty() {
            summary = entry.name.as_str().to_string();
        }
        registry.insert(entry);
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
        if summary.is_empty() {
            summary = entry.name.as_str().to_string();
        }
        registry.insert(entry);
    }

    if registry.is_empty() {
        // Stub mode — see README for details. Lets the harness exercise the
        // full plumbing (ledger, tool registry, criterion eval, scorecard
        // serialization) without a real model behind it.
        let stub = StubProvider::default();
        let entry = ModelEntry {
            name: stub.name().clone(),
            kind: ModelKind::Local,
            model_id: "stub".to_string(),
            priority: 1,
            capabilities: stub.capabilities(),
            cost_per_mtok_in: 0.0,
            cost_per_mtok_out: 0.0,
            provider: Arc::new(stub),
            tool_dialect: Default::default(),
            thinking: false,
        };
        summary = format!("{} (stub mode)", entry.name);
        registry.insert(entry);
    }

    (registry, summary)
}

fn build_tools() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(FsReadTool);
    r.register(FsWriteTool);
    r.register(ShellTool);
    r.register(ApplyPatchTool);
    r.register(GrepTool);
    r.register(GlobTool);
    r.register(UpdatePlanTool);
    r.register(jarvis_tools::ReplaceFileContentTool);
    r.register(jarvis_tools::FsReadManyTool);
    r.register(jarvis_tools::ListDirTool);
    r
}

fn parse_routing(s: &str) -> Result<RoutingPolicy> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "auto" | "" => RoutingPolicy::Auto,
        "local_only" | "local-only" | "local" => RoutingPolicy::LocalOnly,
        "remote_only" | "remote-only" | "remote" => RoutingPolicy::RemoteOnly,
        other => return Err(anyhow!("unknown routing policy `{other}`")),
    })
}

fn print_task_line(r: &jarvis_bench::TaskResult) {
    let (mark, color_on, color_off) = match r.outcome {
        TaskOutcome::Pass => ("OK ", "\x1b[32m", "\x1b[0m"),
        TaskOutcome::Fail => ("FAIL", "\x1b[31m", "\x1b[0m"),
        TaskOutcome::Timeout => ("TIME", "\x1b[33m", "\x1b[0m"),
        TaskOutcome::Error => ("ERR ", "\x1b[35m", "\x1b[0m"),
    };
    let reason = r.failure_reason.as_deref().unwrap_or("");
    println!(
        "  {color_on}[{mark}]{color_off} {id}  steps={steps}  duration={dur}ms  tokens={t_in}/{t_out}  {reason}",
        id = r.id,
        steps = r.steps_taken,
        dur = r.duration_ms,
        t_in = r.tokens_in,
        t_out = r.tokens_out,
    );
}

fn print_summary(sc: &Scorecard) {
    let s = &sc.summary;
    println!();
    println!("Suite: {}", sc.suite);
    println!(
        "  total={}  pass={}  fail={}  timeout={}  error={}  pass_rate={:.2}%",
        s.total,
        s.pass,
        s.fail,
        s.timeout,
        s.error,
        s.pass_rate * 100.0
    );
    println!(
        "  total_duration={}ms  tokens={}/{}  cost=${:.4}",
        s.total_duration_ms, s.total_tokens_in, s.total_tokens_out, s.total_cost_usd
    );
}
