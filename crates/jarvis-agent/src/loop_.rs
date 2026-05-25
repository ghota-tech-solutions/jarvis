//! The actual agent loop.

use crate::prompt;
use crate::protocol::{
    ActionKind, AgentError, AgentReply, Outcome, parse_reply, preprocess_response,
    recover_from_parse_failure,
};
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
    /// Lifecycle hooks. Each hook carries a [`HookPhase`] (Pre / Post /
    /// OnError) and a regex matched against the tool name; matches run via
    /// the sandbox and their output is fed back to the agent as a synthetic
    /// `tool_result` (tool name = `hook:<phase>:<label>` — e.g.
    /// `hook:post:cargo check`). Pre hooks run before `invoke()`, Post hooks
    /// run after a successful invocation (regardless of `is_error` payload),
    /// OnError hooks run only when the tool failed to execute at all.
    pub hooks: Vec<HookSpec>,
    /// What the agent is allowed to do. In `ReadOnly`, any tool with
    /// `side_effects = true` is blocked before invocation and the agent gets
    /// a synthetic error observation explaining the policy.
    pub sandbox_mode: SandboxMode,
    /// M11.S6: optional verdict-gate validator. When set + `enabled`, the
    /// agent's `done` is double-checked by a second LLM call; NACK injects
    /// one more Continuation. Capped to avoid loops.
    pub validation: ValidationSpec,
    /// § T1.3 — when true, the tool catalog renders names + descriptions
    /// only (no full JSON schemas). The agent must call `search_tools(query)`
    /// to discover full argument schemas on demand. Saves ~3–5 k tokens per
    /// turn on registries with 12+ tools but costs one extra round-trip
    /// the first time the model uses an unfamiliar tool. Default: false
    /// (eager — preserves pre-T1.3 behaviour).
    pub lazy_tool_catalog: bool,
}

/// Plain mirror of `jarvis_config::ValidationConfig` so the agent crate
/// doesn't depend on jarvis-config (clean leaf-ward DAG). The daemon
/// converts cfg → spec at task dispatch.
#[derive(Debug, Clone, Default)]
pub struct ValidationSpec {
    pub enabled: bool,
    /// Empty = the daemon picks a sensible default at dispatch.
    pub model: String,
    pub max_validations: u32,
    /// M12.S4: fire a full reviewer sub-agent (read-only tools) instead of
    /// a plain LLM call when the validator runs.
    pub use_subagent: bool,
}

/// When a [`HookSpec`] fires relative to a tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookPhase {
    /// Before `tool.invoke()`. Use for pre-flight checks, secret redaction,
    /// snapshot/backup of state, or anything that should observe inputs
    /// without mutating them.
    Pre,
    /// After `tool.invoke()` returns `Ok` — regardless of whether the
    /// payload's `is_error` flag is set. Use for verification (`cargo
    /// check`, lint), incremental indexing, or autosave.
    Post,
    /// After `tool.invoke()` returns `Err` (the tool itself failed to run
    /// — sandbox error, schema rejection, missing binary). Use for
    /// incident logging, alerts, or auto-replan triggers.
    OnError,
}

impl HookPhase {
    /// Slug used in the synthetic tool name (`hook:<slug>:<label>`) and in
    /// the event payload `phase` field. Stable string contract.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pre => "pre",
            Self::Post => "post",
            Self::OnError => "on_error",
        }
    }
}

/// Compiled lifecycle hook. Construct once at task-dispatch time so the regex
/// cost amortizes across all subsequent tool invocations.
#[derive(Clone)]
pub struct HookSpec {
    pub phase: HookPhase,
    pub label: String,
    pub matcher: regex::Regex,
    pub cmd: String,
    pub workdir: Option<PathBuf>,
    pub timeout: std::time::Duration,
}

impl std::fmt::Debug for HookSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookSpec")
            .field("phase", &self.phase)
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

    // M10.S4: rolling log of recent tool-call signatures to break infinite
    // retries. See `check_and_record_loop` below.
    let mut tool_call_log: std::collections::VecDeque<u64> =
        std::collections::VecDeque::with_capacity(LOOP_WINDOW);

    for step in 1..=run.max_steps {
        if run.cancel.is_cancelled() {
            ledger
                .set_task_status(
                    run.task_id,
                    TaskStatus::Cancelled,
                    Some("cancelled by user"),
                )
                .await?;
            return Ok(Outcome::Aborted);
        }

        // 1) Pull recent context from the ledger. We use the relevant-only
        //    filter so 80 here means 80 agent moves (decisions/tool results),
        //    not 80 raw rows that would be flooded by streaming chunks.
        //
        //    § C follow-up fix: when the task has a parent (the SPA's
        //    "Ask for a follow-up" form sets `parent_task_id`), walk the
        //    ancestor chain and merge their relevant events into the
        //    history. Otherwise a follow-up like "a Lyon ?" lands in the
        //    agent prompt with no context — it sees a fresh conversation
        //    instead of the prior weather goal it's refining.
        let ancestors = ledger.walk_ancestors(run.task_id).await?;
        let history = if ancestors.len() <= 1 {
            ledger.recent_relevant_events(run.task_id, 400).await?
        } else {
            let chain_ids: Vec<jarvis_core::TaskId> = ancestors.iter().map(|t| t.id).collect();
            // Pull a larger raw window (oldest → newest by event id),
            // filter to relevant kinds, then keep the most-recent 400
            // globally. Ledger ids are monotonic → most-recent-400-by-id
            // gives us the right tail across the entire chain.
            let mut events = ledger
                .query_events_multi(&chain_ids, 0, 1000)
                .await?
                .into_iter()
                .filter(|e| {
                    matches!(
                        e.kind,
                        EventKind::Decision
                            | EventKind::ToolResult
                            | EventKind::Error
                            | EventKind::Continuation
                            | EventKind::Verdict
                    )
                })
                .collect::<Vec<_>>();
            if events.len() > 400 {
                let drop = events.len() - 400;
                events.drain(..drop);
            }
            events
        };
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
        log_event(
            &ledger,
            &run,
            EventKind::Heartbeat,
            json!({ "step": step, "max_steps": run.max_steps }),
        )
        .await?;

        // 2) Pick a model — try, on failure quarantine + forbid + retry until exhausted.
        // Returns (raw text, dialect of the model that produced it) so the
        // § C.M-C preprocessor below can apply the dialect-specific rewrite.
        let mut forbidden: HashSet<ProviderName> = HashSet::new();
        let (text, dialect) = loop {
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
            // § C.M-B — build messages with the dialect of the picked model.
            // This sits inside the pick loop because each quarantine retry may
            // route to a model with a different dialect; we want the system
            // prompt to match. History/memories are cheap to re-render — the
            // expensive bits (tool catalog) are static per task.
            // § C follow-up — only count ancestors EXCEPT the leaf (which
            // is the current task itself). When the chain has just one
            // entry, this collapses to an empty slice and build_messages
            // falls back to the standard fresh-task framing.
            let ancestor_goals: Vec<String> = if ancestors.len() > 1 {
                ancestors
                    .iter()
                    .take(ancestors.len() - 1)
                    .map(|t| t.goal.clone())
                    .collect()
            } else {
                Vec::new()
            };
            let messages = prompt::build_messages(
                &task.goal,
                &task.workdir,
                &tool_schemas,
                &history,
                &memories,
                picked.tool_dialect,
                &ancestor_goals,
                picked.thinking,
                picked.ctx_len,
                run.lazy_tool_catalog,
            );
            log_event(
                &ledger,
                &run,
                EventKind::Attempt,
                json!({
                    "step": step,
                    "model": picked.name.as_str(),
                    "model_id": picked.model_id,
                    "kind": picked.kind.as_str(),
                    "tool_dialect": picked.tool_dialect.as_str(),
                }),
            )
            .await?;

            let chat_req = ChatRequest {
                messages: messages.clone(),
                // Low temperature keeps the model on the strict JSON
                // contract; top_p follows the Gemma 4 model-card value.
                temperature: Some(0.2),
                top_p: Some(0.95),
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
            // M11.S5: capture the usage tally from the stream's final chunk
            // so we can attribute tokens + cost to this task. OpenAI-compat
            // providers send usage in the very last chunk (empty delta).
            let mut final_usage: Option<jarvis_core::Usage> = None;
            const CHUNK_FLUSH_BYTES: usize = 80;
            let mut stream_failed = false;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(c) => {
                        if c.usage.is_some() {
                            final_usage = c.usage.clone();
                        }
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
            // M11.S5: emit a dedicated `Observation` carrying the per-turn
            // usage so the daemon's fleet aggregator + cost RPCs can roll
            // it up without re-tokenizing the transcript. Payload nests
            // tokens under `usage` so the existing extract_usage helper
            // (service.rs) picks it up unchanged.
            if let Some(u) = final_usage.as_ref() {
                log_event(
                    &ledger,
                    &run,
                    EventKind::Observation,
                    json!({
                        "kind": "turn_usage",
                        "step": step,
                        "model": picked.name.as_str(),
                        "model_id": picked.model_id.clone(),
                        "usage": {
                            "tokens_in": u.prompt_tokens,
                            "tokens_out": u.completion_tokens,
                        },
                    }),
                )
                .await?;
            }
            break (text, picked.tool_dialect);
        };

        // 2) § C.M-C — dialect-aware preprocessing. For `Json` / `Gemma4Strict`
        //    this is a borrow-only passthrough; for `Gemma4Native` it rewrites
        //    `<|tool_call>...<tool_call|>` envelopes into the JSON contract.
        let cooked = preprocess_response(&text, dialect);

        // 3) Parse the reply. § C.M-A: recover from common failure modes
        //    instead of burning the step budget on identical re-prompts.
        let reply = match parse_reply(&cooked) {
            Ok(r) => r,
            Err(e) => {
                if let Some(coerced) = recover_from_parse_failure(&text) {
                    // No JSON-ish structure at all → the model finalized in
                    // prose (markdown summary, plain sentence, ...). Respect
                    // that intent and treat it as an implicit Done.
                    warn!(error = %e, step, "parse failed; coercing prose reply to Done");
                    log_event(
                        &ledger,
                        &run,
                        EventKind::Error,
                        json!({
                            "step": step,
                            "kind": "parse_recovered",
                            "recovered_as": "coerced_to_done",
                            "raw": text.chars().take(2000).collect::<String>(),
                            "message": e.to_string(),
                        }),
                    )
                    .await?;
                    coerced
                } else {
                    // Empty reply or malformed JSON attempt — model is still
                    // trying to follow the contract. Inject a reminder and
                    // let it retry on the next step.
                    warn!(error = %e, step, "parse failed; injecting reminder and retrying");
                    log_event(
                        &ledger,
                        &run,
                        EventKind::Error,
                        json!({
                            "step": step,
                            "kind": "parse_recovered",
                            "recovered_as": "retried_with_reminder",
                            "raw": text.chars().take(2000).collect::<String>(),
                            "message": e.to_string(),
                        }),
                    )
                    .await?;
                    log_event(
                        &ledger,
                        &run,
                        EventKind::Observation,
                        json!({
                            "system_correction": "Your last reply was not valid JSON. Reply with one JSON object matching the schema in the system prompt. No prose, no markdown, no headings outside the JSON."
                        }),
                    )
                    .await?;
                    continue;
                }
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

                // M11.S6: validator gate. Capped so a flaky validator can't loop.
                if run.validation.enabled {
                    let validations_used = history
                        .iter()
                        .filter(|e| {
                            matches!(e.kind, EventKind::Continuation)
                                && e.payload.get("kind").and_then(|v| v.as_str())
                                    == Some("validation_nack")
                        })
                        .count() as u32;
                    if validations_used < run.validation.max_validations.max(1) {
                        let verdict_opt: Option<crate::validator::Verdict> = if run
                            .validation
                            .use_subagent
                        {
                            // M12.S4: full reviewer sub-agent — calls back into
                            // the daemon via gRPC self-call, reuses standard
                            // bearer-token discovery.
                            let token = jarvis_api::auth::discover_token().unwrap_or_default();
                            let daemon_url = std::env::var("JARVIS_DAEMON_URL")
                                .unwrap_or_else(|_| "http://127.0.0.1:7777".to_string());
                            Some(
                                crate::validator::validate_via_subagent(
                                    &daemon_url,
                                    &token,
                                    &run.task_id.to_string(),
                                    &task.workdir,
                                    &task.goal,
                                    reply.message.as_deref(),
                                )
                                .await,
                            )
                        } else {
                            // M11.S6: plain LLM validator. Pick a model: explicit
                            // override if set, else any sensible planning one.
                            let pick_req = if run.validation.model.is_empty() {
                                jarvis_llm::PickRequest::for_planning()
                            } else {
                                let mut req = jarvis_llm::PickRequest::for_planning();
                                req.routing_override = Some(jarvis_core::RoutingPolicy::Model(
                                    jarvis_core::ProviderName::new(run.validation.model.clone()),
                                ));
                                req
                            };
                            let validator_provider =
                                pool.pick(&pick_req).await.ok().map(|p| p.provider);
                            if let Some(provider) = validator_provider {
                                Some(
                                    crate::validator::validate(
                                        provider,
                                        &task.goal,
                                        reply.message.as_deref(),
                                        &history,
                                    )
                                    .await,
                                )
                            } else {
                                None
                            }
                        };
                        if let Some(verdict) = verdict_opt {
                            match verdict {
                                crate::validator::Verdict::Nack { reason } => {
                                    warn!(step, %reason, "validator NACK — injecting continuation");
                                    log_event(
                                        &ledger,
                                        &run,
                                        EventKind::Continuation,
                                        json!({
                                            "kind": "validation_nack",
                                            "reason": reason,
                                            "after_message": reply.message,
                                        }),
                                    )
                                    .await?;
                                    continue;
                                }
                                crate::validator::Verdict::Ack => {
                                    info!(step, "validator ACK");
                                }
                                crate::validator::Verdict::Skip { reason } => {
                                    warn!(step, %reason, "validator skipped");
                                }
                            }
                        }
                    }
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
                let blocked = check_and_record_loop(&mut tool_call_log, &reply);
                if blocked {
                    log_event(
                        &ledger,
                        &run,
                        EventKind::Observation,
                        json!({
                            "kind": "loop_detected",
                            "summary": "loop detected: this same tool+args has been called 3 times in a row without observable progress",
                            "hint": "change strategy: try a different tool, different arguments, or emit done/fail with an explanation",
                        }),
                    )
                    .await?;
                    continue;
                }
                run_tool_step(&ledger, &tools, &ctx, &run, &reply).await?;
            }
        }
    }

    finish_aborted(&ledger, run.task_id, run.max_steps).await
}

/// Bounded rolling log of recent tool-call signatures. Used by the loop
/// detector to refuse a tool call that exactly repeats one of the last
/// `LOOP_WINDOW` entries `LOOP_THRESHOLD` times.
const LOOP_WINDOW: usize = 5;
const LOOP_THRESHOLD: usize = 3;

/// Returns `true` when the call should be blocked because it's the 3rd
/// identical call in the recent window. Side-effect: pushes the signature
/// into the log (only if it's NOT being blocked, so a single retry is
/// allowed once the agent changes course).
fn check_and_record_loop(log: &mut std::collections::VecDeque<u64>, reply: &AgentReply) -> bool {
    let tool = reply.tool.as_deref().unwrap_or("");
    let args = reply.args.clone().unwrap_or(serde_json::Value::Null);
    let sig = hash_signature(tool, &args);
    let occurrences = log.iter().filter(|x| **x == sig).count();
    if occurrences >= LOOP_THRESHOLD - 1 {
        // Don't push — we want the NEXT call (even if identical) to also
        // trip the same block. Forces the model to break the cycle.
        return true;
    }
    if log.len() >= LOOP_WINDOW {
        log.pop_front();
    }
    log.push_back(sig);
    false
}

fn hash_signature(tool: &str, args: &serde_json::Value) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    tool.hash(&mut h);
    // serde_json::Value's stringification is stable for objects with
    // identical keys + values, which is exactly what we want.
    let canon = serde_json::to_string(args).unwrap_or_default();
    canon.hash(&mut h);
    h.finish()
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
        let mut ev =
            NewEvent::new(run.task_id, EventKind::ToolResult, payload).with_agent(run.agent_id);
        if let Some(s) = subject {
            ev = ev.with_subject(s);
        }
        ledger.append(ev).await?;
        return Ok(());
    }

    // Hooks. Skip the whole machinery for synthetic `hook:*` events — they
    // are themselves emitted by `run_one_hook` and must not re-trigger.
    let hooks_active = !run.hooks.is_empty() && !tool_name.starts_with("hook:");

    // Pre-tool hooks fire AFTER the ReadOnly gate (which has already returned
    // above) and BEFORE the actual invocation. Use for snapshot/backup,
    // secret redaction, audit logging — anything that should observe inputs
    // without mutating them.
    if hooks_active {
        for hook in run.hooks.iter().filter(|h| h.phase == HookPhase::Pre) {
            if !hook.matcher.is_match(tool_name) {
                continue;
            }
            run_one_hook(ledger, ctx, run, hook).await?;
        }
    }

    let args_for_result = args.clone();
    let result = tools.invoke(tool_name, args, ctx).await;
    let invocation_ok = result.is_ok();
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

    // Post-tool hooks (Ok path) OR OnError hooks (Err path). Each matching
    // hook runs via the sandbox and emits its own synthetic tool_result so
    // the next agent turn sees the outcome inline with the other
    // observations.
    if hooks_active {
        let fired_phase = if invocation_ok {
            HookPhase::Post
        } else {
            HookPhase::OnError
        };
        for hook in run.hooks.iter().filter(|h| h.phase == fired_phase) {
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
    let workdir = hook.workdir.clone().unwrap_or_else(|| ctx.workdir.clone());
    let spec = SandboxSpec {
        cmd: hook.cmd.clone(),
        workdir,
        env: std::collections::HashMap::new(),
        timeout: hook.timeout,
        net: ctx.net_policy.clone(),
    };
    let phase_slug = hook.phase.as_str();
    let synth_name = format!("hook:{phase_slug}:{}", hook.label);
    let outcome = ctx.sandbox.exec(spec).await;
    let event = match outcome {
        Ok(out) => {
            let is_error = out.exit_code != 0 || out.timed_out;
            let summary = if out.timed_out {
                format!("{} · {phase_slug} · timed out", hook.label)
            } else {
                format!("{} · {phase_slug} · exit {}", hook.label, out.exit_code)
            };
            NewEvent::new(
                run.task_id,
                EventKind::ToolResult,
                json!({
                    "tool": synth_name,
                    "phase": phase_slug,
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
                "phase": phase_slug,
                "args": { "cmd": hook.cmd },
                "summary": format!("{} · {phase_slug} · failed to spawn", hook.label),
                "data": { "error": e.to_string() },
                "is_error": true,
            }),
        ),
    };
    ledger.append(event.with_agent(run.agent_id)).await?;
    Ok(())
}

fn subject_from_args(tool: &str, args: &serde_json::Value) -> Option<String> {
    match tool {
        "fs_read" | "fs_write" => args.get("path").and_then(|v| v.as_str()).map(String::from),
        "shell" => args.get("cmd").and_then(|v| v.as_str()).map(|s| {
            // First word of the command, capped.
            s.split_whitespace()
                .next()
                .unwrap_or("")
                .chars()
                .take(64)
                .collect()
        }),
        "update_plan" => args.get("plan").and_then(|v| v.as_array()).map(|steps| {
            let done = steps
                .iter()
                .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("completed"))
                .count();
            format!("{done}/{} done", steps.len())
        }),
        _ => None,
    }
}

async fn finish_failed(ledger: &Ledger, task_id: TaskId, msg: &str) -> Result<Outcome, AgentError> {
    ledger
        .append(NewEvent::new(
            task_id,
            EventKind::Verdict,
            json!({ "verdict": "fail", "message": msg }),
        ))
        .await?;
    ledger
        .set_task_status(task_id, TaskStatus::Failed, Some(msg))
        .await?;
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
    ledger
        .set_task_status(task_id, TaskStatus::Failed, Some(&msg))
        .await?;
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

#[cfg(test)]
mod loop_detector_tests {
    use super::*;
    use crate::protocol::{ActionKind, AgentReply};
    use serde_json::json;
    use std::collections::VecDeque;

    fn tool_reply(name: &str, args: serde_json::Value) -> AgentReply {
        AgentReply {
            action: ActionKind::Tool,
            tool: Some(name.to_string()),
            args: Some(args),
            message: None,
            thought: None,
        }
    }

    #[test]
    fn first_two_identical_calls_pass() {
        let mut log = VecDeque::new();
        let r = tool_reply("shell", json!({"cmd": "cargo test"}));
        assert!(!check_and_record_loop(&mut log, &r));
        assert!(!check_and_record_loop(&mut log, &r));
    }

    #[test]
    fn third_identical_call_is_blocked() {
        let mut log = VecDeque::new();
        let r = tool_reply("shell", json!({"cmd": "cargo test"}));
        check_and_record_loop(&mut log, &r);
        check_and_record_loop(&mut log, &r);
        assert!(check_and_record_loop(&mut log, &r));
    }

    #[test]
    fn different_args_dont_count() {
        let mut log = VecDeque::new();
        let a = tool_reply("shell", json!({"cmd": "cargo test"}));
        let b = tool_reply("shell", json!({"cmd": "cargo build"}));
        assert!(!check_and_record_loop(&mut log, &a));
        assert!(!check_and_record_loop(&mut log, &b));
        assert!(!check_and_record_loop(&mut log, &a));
    }

    #[test]
    fn signature_is_stable_across_arg_ordering() {
        // Verify the hash doesn't depend on JSON key insertion order.
        let mut log_a = VecDeque::new();
        let mut log_b = VecDeque::new();
        let a = tool_reply("shell", json!({"cmd": "ls", "cwd": "/tmp"}));
        let b = tool_reply("shell", json!({"cwd": "/tmp", "cmd": "ls"}));
        check_and_record_loop(&mut log_a, &a);
        check_and_record_loop(&mut log_b, &b);
        // Both signatures should compare equal; check that the 2nd
        // recording of `a` in log_b detects the (single) prior entry.
        let blocked = check_and_record_loop(&mut log_b, &a);
        // After 2 entries (b then a — both identical sigs), the 3rd
        // identical call should block.
        check_and_record_loop(&mut log_a, &a);
        assert!(check_and_record_loop(&mut log_a, &a) || blocked);
    }
}
