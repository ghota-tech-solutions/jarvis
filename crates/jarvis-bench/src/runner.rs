//! Per-task runner — wires fixtures, agent loop, criterion evaluation,
//! and ledger-derived metrics into a single `TaskResult`.

use crate::scorecard::{TaskOutcome, TaskResult};
use crate::suite::{CriterionResult, Defaults, Task};
use anyhow::{Context, Result};
use jarvis_agent::{AgentRun, ValidationSpec, run_agent};
use jarvis_core::{AgentId, RequiredCapabilities, RoutingPolicy, SandboxMode, TaskKind};
use jarvis_ledger::{EventKind, Ledger};
use jarvis_llm::LlmPool;
use jarvis_sandbox::{NativeSandbox, NetPolicy};
use jarvis_tools::{ToolCtx, ToolRegistry};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Options that apply to every task in the suite.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub routing: RoutingPolicy,
    pub sandbox_mode: SandboxMode,
    pub keep_tempdir: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            routing: RoutingPolicy::Auto,
            sandbox_mode: SandboxMode::WorkspaceWrite,
            keep_tempdir: false,
        }
    }
}

/// Run one task end-to-end and produce a `TaskResult`.
///
/// Note: a single bench-local ledger is created per task to keep token /
/// step accounting cleanly scoped. Errors creating fixtures or the ledger
/// surface as `TaskOutcome::Error` — never panic the bench.
pub async fn run_task(
    task: &Task,
    defaults: &Defaults,
    opts: &RunOptions,
    pool: Arc<LlmPool>,
    tools: ToolRegistry,
) -> TaskResult {
    let start = Instant::now();
    let max_steps = task.effective_max_steps(defaults);
    let timeout = std::time::Duration::from_secs(task.effective_timeout_s(defaults));

    // 1) tempdir + fixtures
    let temp = match prepare_tempdir(task) {
        Ok(t) => t,
        Err(e) => {
            return TaskResult {
                id: task.id.clone(),
                outcome: TaskOutcome::Error,
                steps_taken: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                failure_reason: Some(format!("fixtures: {e:#}")),
            };
        }
    };
    let workdir = temp.path().to_path_buf();
    debug!(task = %task.id, workdir = %workdir.display(), "task tempdir ready");

    // 2) ledger (per-task, in the tempdir)
    let ledger_path = workdir.join(".jarvis-bench-ledger.sqlite");
    let ledger = match Ledger::open(&ledger_path).await {
        Ok(l) => l,
        Err(e) => {
            return TaskResult {
                id: task.id.clone(),
                outcome: TaskOutcome::Error,
                steps_taken: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                failure_reason: Some(format!("ledger open: {e}")),
            };
        }
    };

    // 3) create the task row (run_agent reads this back)
    let task_record = match ledger
        .create_task(&task.goal, &workdir.display().to_string(), None)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            return TaskResult {
                id: task.id.clone(),
                outcome: TaskOutcome::Error,
                steps_taken: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                failure_reason: Some(format!("create_task: {e}")),
            };
        }
    };

    // 4) build AgentRun + ToolCtx + run with timeout
    let cancel = CancellationToken::new();
    let run = AgentRun {
        task_id: task_record.id,
        workdir: workdir.clone(),
        max_steps,
        agent_id: AgentId::new(),
        cancel: cancel.clone(),
        routing: opts.routing.clone(),
        required: RequiredCapabilities::default(),
        kind: TaskKind::Planning,
        continuation_budget: 0,
        hooks: Vec::new(),
        sandbox_mode: opts.sandbox_mode,
        validation: ValidationSpec::default(),
        lazy_tool_catalog: false,
    };
    let ctx = ToolCtx {
        workdir: workdir.clone(),
        cancel: Arc::new(cancel.clone()),
        sandbox: Arc::new(NativeSandbox),
        net_policy: NetPolicy::Full,
        current_task_id: task_record.id.to_string(),
        editor: None,
    };

    let agent_fut = run_agent(run, pool, ledger.clone(), tools, ctx);
    let run_outcome = tokio::time::timeout(timeout, agent_fut).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    // 5) gather metrics from the ledger (best-effort)
    let metrics = collect_metrics(&ledger, task_record.id).await;

    // 6) classify outcome
    let (outcome, failure_reason) = match run_outcome {
        Err(_) => {
            cancel.cancel();
            (
                TaskOutcome::Timeout,
                Some(format!("timed out after {timeout:?}")),
            )
        }
        Ok(Err(e)) => (TaskOutcome::Error, Some(format!("agent error: {e}"))),
        Ok(Ok(_outcome)) => match task.success.evaluate(&workdir).await {
            CriterionResult::Pass => (TaskOutcome::Pass, None),
            CriterionResult::Fail { reason } => (TaskOutcome::Fail, Some(reason)),
        },
    };

    // 7) Optionally keep the tempdir for debugging.
    if opts.keep_tempdir {
        let kept = temp.keep();
        warn!(task = %task.id, path = %kept.display(), "keep-tempdir: not cleaning up");
    }

    TaskResult {
        id: task.id.clone(),
        outcome,
        steps_taken: metrics.steps,
        duration_ms: elapsed_ms,
        tokens_in: metrics.tokens_in,
        tokens_out: metrics.tokens_out,
        cost_usd: 0.0,
        failure_reason,
    }
}

fn prepare_tempdir(task: &Task) -> Result<TempDir> {
    let temp = tempfile::Builder::new()
        .prefix("jarvis-bench-")
        .tempdir()
        .context("creating tempdir")?;
    for (rel, content) in &task.fixtures {
        let target = temp.path().join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir -p `{}`", parent.display()))?;
        }
        std::fs::write(&target, content)
            .with_context(|| format!("write fixture `{}`", target.display()))?;
    }
    Ok(temp)
}

#[derive(Debug, Default)]
struct Metrics {
    steps: u32,
    tokens_in: u64,
    tokens_out: u64,
}

async fn collect_metrics(ledger: &Ledger, task_id: jarvis_core::TaskId) -> Metrics {
    // We pull every event for the task. A single bench task usually emits a
    // handful so the unbounded query is fine.
    let events = match ledger.query_events(Some(task_id), 0, 10_000).await {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "metrics: failed to query events");
            return Metrics::default();
        }
    };
    let mut m = Metrics::default();
    for ev in events {
        match ev.kind {
            EventKind::Decision => m.steps += 1,
            EventKind::Observation => {
                // M11.S5: turn-usage rolls are emitted as Observation with
                // `kind == "turn_usage"` and a nested `usage` object.
                if ev.payload.get("kind").and_then(|v| v.as_str()) == Some("turn_usage")
                    && let Some(u) = ev.payload.get("usage")
                {
                    if let Some(t) = u.get("tokens_in").and_then(|v| v.as_u64()) {
                        m.tokens_in += t;
                    }
                    if let Some(t) = u.get("tokens_out").and_then(|v| v.as_u64()) {
                        m.tokens_out += t;
                    }
                }
            }
            _ => {}
        }
    }
    m
}

/// Tiny convenience for the CLI surface: short text describing where a task
/// landed.
pub fn outcome_label(out: TaskOutcome) -> &'static str {
    out.as_str()
}

// Make sure we don't warn about unused helpers if downstream changes.
#[allow(dead_code)]
fn _ensure_path_traits<P: AsRef<Path>>(_p: P) {}
