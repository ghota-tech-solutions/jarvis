//! The actual agent loop.

use crate::prompt;
use crate::protocol::{parse_reply, ActionKind, AgentError, AgentReply, Outcome};
use futures_util::StreamExt;
use jarvis_core::{
    AgentId, ChatRequest, ProviderName, RequiredCapabilities, RoutingPolicy, TaskId, TaskKind,
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

        // 1) Pull recent context from the ledger.
        let history = ledger.recent_events(run.task_id, 40).await?;
        let messages = prompt::build_messages(&task.goal, &task.workdir, &tool_schemas, &history);

        ledger
            .append(
                NewEvent::new(
                    run.task_id,
                    EventKind::Heartbeat,
                    json!({ "step": step, "max_steps": run.max_steps }),
                )
                .with_agent(run.agent_id),
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
                    let payload = json!({ "step": step, "kind": "pool", "error": e.to_string() });
                    ledger
                        .append(
                            NewEvent::new(run.task_id, EventKind::Error, payload)
                                .with_agent(run.agent_id),
                        )
                        .await?;
                    return finish_failed(&ledger, run.task_id, &format!("router: {e}")).await;
                }
            };
            ledger
                .append(
                    NewEvent::new(
                        run.task_id,
                        EventKind::Attempt,
                        json!({
                            "step": step,
                            "model": picked.name.as_str(),
                            "model_id": picked.model_id,
                            "kind": picked.kind.as_str(),
                        }),
                    )
                    .with_agent(run.agent_id),
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
                    ledger
                        .append(
                            NewEvent::new(
                                run.task_id,
                                EventKind::Error,
                                json!({"step": step, "model": picked.name.as_str(), "kind": "open_stream", "error": e.to_string()}),
                            )
                            .with_agent(run.agent_id),
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
                                ledger
                                    .append(
                                        NewEvent::new(run.task_id, EventKind::LlmChunk, payload)
                                            .with_agent(run.agent_id),
                                    )
                                    .await?;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(model = %picked.name, error = %e, "stream error; quarantining and retrying");
                        pool.record_failure(&picked.name).await;
                        forbidden.insert(picked.name.clone());
                        ledger
                            .append(
                                NewEvent::new(
                                    run.task_id,
                                    EventKind::Error,
                                    json!({"step": step, "model": picked.name.as_str(), "kind": "stream", "error": e.to_string()}),
                                )
                                .with_agent(run.agent_id),
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
                ledger
                    .append(
                        NewEvent::new(
                            run.task_id,
                            EventKind::LlmChunk,
                            json!({"step": step, "seq": chunk_seq, "delta": chunk_buffer}),
                        )
                        .with_agent(run.agent_id),
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
                ledger
                    .append(
                        NewEvent::new(
                            run.task_id,
                            EventKind::Error,
                            json!({
                                "step": step,
                                "kind": "parse_reply",
                                "raw": text.chars().take(2000).collect::<String>(),
                                "message": e.to_string(),
                            }),
                        )
                        .with_agent(run.agent_id),
                    )
                    .await?;
                // Push a synthetic observation telling the model how to reply, then loop.
                ledger
                    .append(
                        NewEvent::new(
                            run.task_id,
                            EventKind::Observation,
                            json!({
                                "system_correction": "Your last reply was not valid JSON. Reply with one JSON object matching the schema in the system prompt."
                            }),
                        )
                        .with_agent(run.agent_id),
                    )
                    .await?;
                continue;
            }
        };

        // 3) Log the decision.
        ledger
            .append(
                NewEvent::new(
                    run.task_id,
                    EventKind::Decision,
                    serde_json::to_value(&reply).unwrap_or(json!(null)),
                )
                .with_agent(run.agent_id),
            )
            .await?;

        // 4) Act.
        match reply.action {
            ActionKind::Done => {
                ledger
                    .append(
                        NewEvent::new(
                            run.task_id,
                            EventKind::Verdict,
                            json!({"verdict":"pass", "message": reply.message }),
                        )
                        .with_agent(run.agent_id),
                    )
                    .await?;
                ledger
                    .set_task_status(run.task_id, TaskStatus::Completed, None)
                    .await?;
                info!(step, "agent: done");
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
