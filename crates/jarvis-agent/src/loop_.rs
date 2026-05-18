//! The actual agent loop.

use crate::prompt;
use crate::protocol::{parse_reply, ActionKind, AgentError, AgentReply, Outcome};
use futures_util::StreamExt;
use jarvis_core::{
    AgentId, ChatRequest, ProviderName, RequiredCapabilities, RoutingPolicy, SandboxMode, TaskId,
    TaskKind,
};
use jarvis_ledger::{EventKind, Ledger, NewEvent, TaskStatus};
use jarvis_llm::{LlmPool, PickRequest};
use jarvis_tools::{ToolCtx, ToolRegistry};
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

/// The agent loop accepts a pre-built `ToolCtx` so the daemon can wire in the
/// appropriate sandbox backend + network policy + workdir at task dispatch.

#[derive(Debug, Clone)]
pub struct AgentRun {
    pub task_id: TaskId,
    pub workdir: PathBuf,
    pub max_steps: u32,
    pub agent_id: AgentId,
    pub cancel: CancellationToken,
    /// Routing policy for this task. Default = Auto.
    pub routing: RoutingPolicy,
    /// Capabilities the agent's planner needs.
    pub required: RequiredCapabilities,
    /// Kind hint for the router's auto-rules.
    pub kind: TaskKind,
    /// How many times we may force a continuation audit after `done`. The first
    /// audit catches premature completion; subsequent ones catch the model
    /// re-declaring `done` without new evidence. 0 = no autopilot.
    pub continuation_budget: u32,
    /// Post-tool verification hooks. Each hook's regex is matched against the
    /// tool name; matches run via the sandbox and their output is fed back to
    /// the agent as a synthetic `tool_result` (tool name = `hook:<label>`).
    pub hooks: Vec<HookSpec>,
    /// What the agent is allowed to do. In `ReadOnly`, any tool with
    /// `side_effects = true` is blocked before invocation and the agent gets
    /// a synthetic error observation explaining the policy.
    pub sandbox_mode: SandboxMode,
}

/// Compiled post-tool hook. Construct once at task-dispatch time so the regex
/// cost amortizes across all subsequent tool invocations.
#[derive(Clone)]
pub struct HookSpec {
    pub label: String,
    pub matcher: regex::Regex,
    pub cmd: String,
    pub workdir: Option<PathBuf>,
    pub timeout: std::time::Duration,
}

impl std::fmt::Debug for HookSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookSpec")
            .field("label", &self.label)
            .field("matcher", &self.matcher.as_str())
            .field("cmd", &self.cmd)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Append a tagged event for this task/agent in one line. Centralizes the
/// boilerplate of `NewEvent::new(...).with_agent(agent_id)` so the call sites
/// in the agent loop stay legible.
async fn log_event(
    ledger: &Ledger,
    run: &AgentRun,
    kind: EventKind,
    payload: serde_json::Value,
) -> Result<(), AgentError> {
    ledger
        .append(NewEvent::new(run.task_id, kind, payload).with_agent(run.agent_id))
        .await?;
    Ok(())
}

#[instrument(skip(pool, ledger, tools, ctx), fields(task = %run.task_id, agent = %run.agent_id))]
pub async fn run_agent(
    run: AgentRun,
    pool: Arc<LlmPool>,
    ledger: Ledger,
    tools: ToolRegistry,
    ctx: ToolCtx,
) -> Result<Outcome, AgentError> {
    let task = ledger.get_task(run.task_id).await?;
    ledger
        .set_task_status(run.task_id, TaskStatus::Running, None)
        .await?;

    let tool_schemas = tools.schemas();

    for step in 1..=run.max_steps {
        if run.cancel.is_cancelled() {
            ledger
                .set_task_status(run.task_id, TaskStatus::Cancelled, Some("cancelled by user"))
                .await?;
            return Ok(Outcome::Aborted);
        }

        // 1) Pull recent context from the ledger. We use the relevant-only
        //    query so 80 here means 80 agent moves (decisions/tool results),
        //    not 80 raw rows that would be flooded by streaming chunks.
        let history = ledger.recent_relevant_events(run.task_id, 80).await?;
        // M9: pull active memories for this workdir + globals. Surface as a
        // dedicated system message inside build_messages. Best-effort — if
        // the ledger errors we still proceed with an empty list.
        let memories = ledger
            .active_memories_for_workdir(&task.workdir)
            .await
            .unwrap_or_default();
        // Bump usage counters so the user can see which memories the agent
        // is actually relying on. Fire-and-forget.
        for m in &memories {
            let _ = ledger.increment_memory_usage(m.id).await;
        }
        let messages = prompt::build_messages(
            &task.goal,
            &task.workdir,
            &tool_schemas,
            &history,
            &memories,
        );

        log_event(
            &ledger,
            &run,
            EventKind::Heartbeat,
            json!({ "step": step, "max_steps": run.max_steps }),
        )
        .await?;

        // 2) Pick a model — try, on failure quarantine + forbid + retry until exhausted.
        let mut forbidden: HashSet<ProviderName> = HashSet::new();
        let text = loop {
            let pick_req = PickRequest {
                required: run.required.clone(),
                kind: run.kind,
                estimated_tokens: 0,
                routing_override: Some(run.routing.clone()),
                forbidden: forbidden.clone(),
            };
            let picked = match pool.pick(&pick_req).await {
                Ok(p) => p,
                Err(e) => {
                    log_event(
                        &ledger,
                        &run,
                        EventKind::Error,
                        json!({ "step": step, "kind": "pool", "error": e.to_string() }),
                    )
                    .await?;
                    return finish_failed(&ledger, run.task_id, &format!("router: {e}")).await;
                }
            };
            log_event(
                &ledger,
                &run,
                EventKind::Attempt,
                json!({
                    "step": step,
                    "model": picked.name.as_str(),
                    "model_id": picked.model_id,
                    "kind": picked.kind.as_str(),
                }),
            )
            .await?;

            let chat_req = ChatRequest {
                messages: messages.clone(),
                temperature: Some(0.2),
                max_tokens: Some(1024),
                stream: true,
            };

            let mut stream = match picked.provider.complete_stream(chat_req).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(model = %picked.name, error = %e, "stream open failed; quarantining and retrying");
                    pool.record_failure(&picked.name).await;
                    forbidden.insert(picked.name.clone());
                    log_event(
                        &ledger,
                        &run,
                        EventKind::Error,
                        json!({"step": step, "model": picked.name.as_str(), "kind": "open_stream", "error": e.to_string()}),
                    )
                    .await?;
                    continue;
                }
            };

            let mut text = String::new();
            let mut chunk_buffer = String::new();
            let mut chunk_seq: u32 = 0;
            const CHUNK_FLUSH_BYTES: usize = 80;
            let mut stream_failed = false;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(c) => {
                        if !c.delta.is_empty() {
                            text.push_str(&c.delta);
                            chunk_buffer.push_str(&c.delta);
                            // Flush as a llm_chunk event so clients can render
                            // tokens live. We batch to keep ledger volume sane.
                            if chunk_buffer.len() >= CHUNK_FLUSH_BYTES
                                || chunk_buffer.contains('\n')
                            {
                                chunk_seq += 1;
                                let payload = json!({
                                    "step": step,
                                    "seq": chunk_seq,
                                    "delta": chunk_buffer,
                                });
                                chunk_buffer.clear();
                                log_event(&ledger, &run, EventKind::LlmChunk, payload).await?;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(model = %picked.name, error = %e, "stream error; quarantining and retrying");
                        pool.record_failure(&picked.name).await;
                        forbidden.insert(picked.name.clone());
                        log_event(
                            &ledger,
                            &run,
                            EventKind::Error,
                            json!({"step": step, "model": picked.name.as_str(), "kind": "stream", "error": e.to_string()}),
                        )
                        .await?;
                        stream_failed = true;
                        break;
                    }
                }
            }
            // Flush any trailing buffer.
            if !chunk_buffer.is_empty() {
                chunk_seq += 1;
                log_event(
                    &ledger,
                    &run,
                    EventKind::LlmChunk,
                    json!({"step": step, "seq": chunk_seq, "delta": chunk_buffer}),
                )
                .await?;
            }
            if stream_failed {
                continue;
            }
            pool.record_success(&picked.name).await;
            break text;
        };

        // 2) Parse the reply.
        let reply = match parse_reply(&text) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, step, "could not parse LLM reply; injecting correction");
                log_event(
                    &ledger,
                    &run,
                    EventKind::Error,
                    json!({
                        "step": step,
                        "kind": "parse_reply",
                        "raw": text.chars().take(2000).collect::<String>(),
                        "message": e.to_string(),
                    }),
                )
                .await?;
                // Push a synthetic observation telling the model how to reply, then loop.
                log_event(
                    &ledger,
                    &run,
                    EventKind::Observation,
                    json!({
                        "system_correction": "Your last reply was not valid JSON. Reply with one JSON object matching the schema in the system prompt."
                    }),
                )
                .await?;
                continue;
            }
        };

        // 3) Log the decision.
        log_event(
            &ledger,
            &run,
            EventKind::Decision,
            serde_json::to_value(&reply).unwrap_or(json!(null)),
        )
        .await?;

        // 4) Act.
        match reply.action {
            ActionKind::Done => {
                // Count prior continuation audits in this task's history.
                let continuations_used = history
                    .iter()
                    .filter(|e| matches!(e.kind, EventKind::Continuation))
                    .count() as u32;
                // Only force an audit if the agent actually did work (tool calls).
                // Informational answers (no tools called) bypass the audit since
                // there is no environment state to verify against.
                let did_tool_work = history
                    .iter()
                    .any(|e| matches!(e.kind, EventKind::ToolResult));
                if did_tool_work && continuations_used < run.continuation_budget {
                    let attempt = continuations_used + 1;
                    let budget = run.continuation_budget;
                    info!(step, attempt, budget, "agent: forcing continuation audit");
                    log_event(
                        &ledger,
                        &run,
                        EventKind::Continuation,
                        json!({
                            "attempt": attempt,
                            "budget": budget,
                            "after_message": reply.message,
                        }),
                    )
                    .await?;
                    // Loop back: next iteration will rebuild messages with the
                    // continuation event rendered as a user audit prompt.
                    continue;
                }
                log_event(
                    &ledger,
                    &run,
                    EventKind::Verdict,
                    json!({"verdict":"pass", "message": reply.message }),
                )
                .await?;
                ledger
                    .set_task_status(run.task_id, TaskStatus::Completed, None)
                    .await?;
                info!(step, "agent: done");
                // M9: propose long-term memory candidates from this task.
                // Best-effort — failures are logged inside the extractor and
                // never propagate. Spawned because the extractor makes an
                // LLM call that the user shouldn't wait on.
                let ledger_clone = ledger.clone();
                let pool_clone = pool.clone();
                let task_id = run.task_id;
                let workdir = task.workdir.clone();
                let goal = task.goal.clone();
                tokio::spawn(async move {
                    let pick_req = jarvis_llm::PickRequest::for_planning();
                    let provider = match pool_clone.pick(&pick_req).await {
                        Ok(p) => p.provider,
                        Err(e) => {
                            warn!(error = %e, "memory extractor: no model available");
                            return;
                        }
                    };
                    let ids = crate::memory_extractor::extract_for_task(
                        &ledger_clone,
                        provider,
                        task_id,
                        &workdir,
                        &goal,
                    )
                    .await;
                    if !ids.is_empty() {
                        info!(count = ids.len(), task = %task_id, "memory candidates proposed");
                    }
                });
                return Ok(Outcome::Done);
            }
            ActionKind::Fail => {
                let msg = reply.message.unwrap_or_else(|| "agent gave up".to_string());
                return finish_failed(&ledger, run.task_id, &msg).await;
            }
            ActionKind::Tool => {
                run_tool_step(&ledger, &tools, &ctx, &run, &reply).await?;
            }
        }
    }

    finish_aborted(&ledger, run.task_id, run.max_steps).await
}

async fn run_tool_step(
    ledger: &Ledger,
    tools: &ToolRegistry,
    ctx: &ToolCtx,
    run: &AgentRun,
    reply: &AgentReply,
) -> Result<(), AgentError> {
    let tool_name = reply
        .tool
        .as_deref()
        .ok_or(AgentError::MissingField("tool"))?;
    let args = reply.args.clone().unwrap_or(json!({}));
    let subject = subject_from_args(tool_name, &args);

    ledger
        .append(
            NewEvent::new(
                run.task_id,
                EventKind::ToolCall,
                json!({ "tool": tool_name, "args": args.clone() }),
            )
            .with_agent(run.agent_id)
            .maybe_subject(subject.clone()),
        )
        .await?;

    // Sandbox mode gate: ReadOnly blocks any tool whose schema declares
    // side_effects=true BEFORE the tool ever runs. The agent gets a clear
    // observation back so it can either pick a read-only alternative or
    // surface the limitation in its verdict.
    if matches!(run.sandbox_mode, SandboxMode::ReadOnly)
        && tools
            .get(tool_name)
            .map(|t| t.schema().side_effects)
            .unwrap_or(false)
    {
        let payload = json!({
            "tool": tool_name,
            "args": args.clone(),
            "summary": "blocked: sandbox mode is read_only",
            "data": {
                "error": "sandbox_mode=read_only",
                "hint": "this tool would mutate state — pick a read-only tool (fs_read, grep, glob) or report the limitation in your verdict",
            },
            "is_error": true,
        });
        let mut ev = NewEvent::new(run.task_id, EventKind::ToolResult, payload)
            .with_agent(run.agent_id);
        if let Some(s) = subject {
            ev = ev.with_subject(s);
        }
        ledger.append(ev).await?;
        return Ok(());
    }

    let args_for_result = args.clone();
    let result = tools.invoke(tool_name, args, ctx).await;
    let event = match result {
        Ok(out) => {
            // A tool that ran and returned a non-zero exit code (or set
            // is_error=true) is still a successful INVOCATION — we keep the
            // event as ToolResult and let `is_error` inside the payload drive
            // the red-card rendering. Reserve EventKind::Error for failures
            // where the tool couldn't be executed at all (Err branch below).
            NewEvent::new(
                run.task_id,
                EventKind::ToolResult,
                json!({
                    "tool": tool_name,
                    "args": args_for_result,
                    "summary": out.summary,
                    "data": out.data,
                    "is_error": out.is_error,
                }),
            )
        }
        Err(e) => NewEvent::new(
            run.task_id,
            EventKind::Error,
            json!({
                "tool": tool_name,
                "args": args_for_result,
                "message": format!("tool `{tool_name}` failed to run: {e}"),
            }),
        ),
    };
    let mut event = event.with_agent(run.agent_id);
    if let Some(s) = subject {
        event = event.with_subject(s);
    }
    ledger.append(event).await?;

    // Post-tool hooks. Each matching hook runs via the sandbox and emits its
    // own synthetic tool_result so the next agent turn sees the verification
    // outcome inline with the other observations. We never run hooks for
    // synthetic hook events themselves (the tool_name prefix prevents that).
    if !run.hooks.is_empty() && !tool_name.starts_with("hook:") {
        for hook in &run.hooks {
            if !hook.matcher.is_match(tool_name) {
                continue;
            }
            run_one_hook(ledger, ctx, run, hook).await?;
        }
    }
    Ok(())
}

async fn run_one_hook(
    ledger: &Ledger,
    ctx: &ToolCtx,
    run: &AgentRun,
    hook: &HookSpec,
) -> Result<(), AgentError> {
    use jarvis_sandbox::SandboxSpec;
    let workdir = hook
        .workdir
        .clone()
        .unwrap_or_else(|| ctx.workdir.clone());
    let spec = SandboxSpec {
        cmd: hook.cmd.clone(),
        workdir,
        env: std::collections::HashMap::new(),
        timeout: hook.timeout,
        net: ctx.net_policy.clone(),
    };
    let synth_name = format!("hook:{}", hook.label);
    let outcome = ctx.sandbox.exec(spec).await;
    let event = match outcome {
        Ok(out) => {
            let is_error = out.exit_code != 0 || out.timed_out;
            let summary = if out.timed_out {
                format!("{} · timed out", hook.label)
            } else {
                format!("{} · exit {}", hook.label, out.exit_code)
            };
            NewEvent::new(
                run.task_id,
                EventKind::ToolResult,
                json!({
                    "tool": synth_name,
                    "args": { "cmd": hook.cmd },
                    "summary": summary,
                    "data": {
                        "exit_code": out.exit_code,
                        "stdout": jarvis_core::clip(&out.stdout, 8 * 1024),
                        "stderr": jarvis_core::clip(&out.stderr, 8 * 1024),
                        "backend": out.backend,
                        "timed_out": out.timed_out,
                    },
                    "is_error": is_error,
                }),
            )
        }
        Err(e) => NewEvent::new(
            run.task_id,
            EventKind::ToolResult,
            json!({
                "tool": synth_name,
                "args": { "cmd": hook.cmd },
                "summary": format!("{} failed to spawn", hook.label),
                "data": { "error": e.to_string() },
                "is_error": true,
            }),
        ),
    };
    ledger
        .append(event.with_agent(run.agent_id))
        .await?;
    Ok(())
}

fn subject_from_args(tool: &str, args: &serde_json::Value) -> Option<String> {
    match tool {
        "fs_read" | "fs_write" => args.get("path").and_then(|v| v.as_str()).map(String::from),
        "shell" => args.get("cmd").and_then(|v| v.as_str()).map(|s| {
            // First word of the command, capped.
            s.split_whitespace().next().unwrap_or("").chars().take(64).collect()
        }),
        _ => None,
    }
}

async fn finish_failed(
    ledger: &Ledger,
    task_id: TaskId,
    msg: &str,
) -> Result<Outcome, AgentError> {
    ledger
        .append(NewEvent::new(
            task_id,
            EventKind::Verdict,
            json!({ "verdict": "fail", "message": msg }),
        ))
        .await?;
    ledger.set_task_status(task_id, TaskStatus::Failed, Some(msg)).await?;
    Ok(Outcome::Failed)
}

async fn finish_aborted(
    ledger: &Ledger,
    task_id: TaskId,
    max_steps: u32,
) -> Result<Outcome, AgentError> {
    let msg = format!("step budget exhausted ({max_steps})");
    ledger
        .append(NewEvent::new(
            task_id,
            EventKind::Verdict,
            json!({ "verdict": "aborted", "message": msg.clone() }),
        ))
        .await?;
    ledger.set_task_status(task_id, TaskStatus::Failed, Some(&msg)).await?;
    Err(AgentError::BudgetExhausted(max_steps))
}

// Tiny extension trait so we can chain `maybe_subject(Option<String>)` ergonomically above.
trait MaybeSubject {
    fn maybe_subject(self, s: Option<String>) -> Self;
}

impl MaybeSubject for NewEvent {
    fn maybe_subject(self, s: Option<String>) -> Self {
        match s {
            Some(v) => self.with_subject(v),
            None => self,
        }
    }
}
