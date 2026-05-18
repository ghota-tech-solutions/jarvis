//! Memory extraction — post-verdict candidate proposals.
//!
//! Called by the agent loop right after a `verdict` event is appended,
//! provided the verdict was a `pass`. The extractor reads the last N
//! relevant events (decisions + tool_results + the verdict itself) and
//! asks the LLM to propose between 0 and 3 short memory candidates.
//!
//! Candidates land in the ledger as `status='candidate'`. The user later
//! promotes them via the SPA's memory sidebar (M9.S3); promoted memories
//! get injected into the system prompt of future tasks in the same
//! workdir.
//!
//! This module deliberately treats the LLM call as best-effort:
//! - timeouts, parsing failures, or empty proposals are non-fatal,
//! - duplicates against existing memories (same workdir + similar text)
//!   are dropped server-side before insertion.

use jarvis_core::{ChatMessage, ChatRequest, LlmProvider, TaskId};
use jarvis_ledger::{
    EventKind, EventRecord, Ledger, MemoryKind, MemoryScope, MemoryStatus, NewMemory,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, warn};

const MAX_CANDIDATES_PER_TASK: usize = 3;
const RELEVANT_EVENT_BUDGET: u32 = 12;

#[derive(Debug, Deserialize)]
struct LlmCandidate {
    kind: String,      // "pattern" | "preference" | "fact"
    text: String,
    scope: Option<String>, // "workdir" | "global"; defaults to workdir
}

/// Build the prompt + call the LLM. Returns the raw response text — the
/// caller parses + sanitises.
fn build_request(events: &[EventRecord], goal: &str) -> ChatRequest {
    let system = r#"
You are a memory extractor for an autonomous coding agent. Read the
agent's recent decisions and outcomes for ONE task and propose between 0
and 3 short long-lived memories that would help on FUTURE tasks in this
codebase.

A memory is one of:
- "pattern"     — a recurring approach the agent should re-use (e.g. "for
                  rust build errors, prefer `cargo check` over `cargo build`
                  to iterate faster")
- "preference"  — a user style or constraint (e.g. "user prefers French
                  in commit message bodies")
- "fact"        — a project invariant (e.g. "the main branch is `develop`")

Rules:
- Only propose things that are GENERALIZABLE — drop one-off observations.
- Keep each text under 200 characters, written in the IMPERATIVE
  ("Always X", "Prefer Y", "Never Z").
- Default `scope` to "workdir". Use "global" only for facts about the
  user, never about a specific repo.
- Output STRICT JSON: an array of objects { "kind", "text", "scope"? }.
  Empty array is fine. No prose, no markdown fences.
"#;

    let mut history = String::with_capacity(2048);
    history.push_str(&format!("Goal: {goal}\n\nRecent events:\n"));
    for e in events {
        let kind = e.kind.as_str();
        let pay = serde_json::to_string(&e.payload).unwrap_or_default();
        let pay = if pay.len() > 400 {
            format!("{}…", &pay[..400])
        } else {
            pay
        };
        history.push_str(&format!("- {kind}: {pay}\n"));
    }

    ChatRequest {
        messages: vec![
            ChatMessage::system(system.trim().to_string()),
            ChatMessage::user(history),
        ],
        temperature: Some(0.2),
        max_tokens: Some(400),
        stream: false,
    }
}

fn parse_candidates(raw: &str) -> Vec<LlmCandidate> {
    // Be tolerant — the LLM occasionally wraps the JSON in markdown fences.
    let trimmed = raw.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_end_matches("```").trim())
        .unwrap_or(trimmed);
    match serde_json::from_str::<Vec<LlmCandidate>>(json) {
        Ok(v) => v,
        Err(e) => {
            debug!(error = %e, raw_len = raw.len(), "memory extractor: parse failed");
            Vec::new()
        }
    }
}

fn sanitise(c: LlmCandidate, default_scope_value: &str) -> Option<NewMemory> {
    use std::str::FromStr;
    let kind = MemoryKind::from_str(&c.kind).ok()?;
    let scope = c
        .scope
        .as_deref()
        .and_then(|s| MemoryScope::from_str(s).ok())
        .unwrap_or(MemoryScope::Workdir);
    let text = c.text.trim();
    if text.len() < 8 || text.len() > 240 {
        return None;
    }
    let scope_value = if matches!(scope, MemoryScope::Workdir) {
        default_scope_value.to_string()
    } else {
        String::new()
    };
    Some(NewMemory {
        scope,
        scope_value,
        kind,
        text: text.to_string(),
        status: MemoryStatus::Candidate,
        source_task_id: None, // caller sets this
    })
}

/// Extract candidate memories for a finished task. Returns the IDs of the
/// memories that were inserted. Errors are logged + swallowed — this is
/// best-effort and must not block the agent loop.
pub async fn extract_for_task(
    ledger: &Ledger,
    provider: Arc<dyn LlmProvider>,
    task_id: TaskId,
    workdir: &str,
    goal: &str,
) -> Vec<i64> {
    let events = match ledger.recent_relevant_events(task_id, RELEVANT_EVENT_BUDGET).await {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "memory extractor: cannot load events");
            return Vec::new();
        }
    };
    if !events.iter().any(|e| {
        e.kind == EventKind::Verdict
            && e.payload
                .get("verdict")
                .and_then(|v| v.as_str())
                .map(|s| s == "pass" || s == "done")
                .unwrap_or(false)
    }) {
        // No pass verdict → no extraction.
        return Vec::new();
    }

    let req = build_request(&events, goal);
    let resp = match provider.complete(req).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "memory extractor: LLM call failed");
            return Vec::new();
        }
    };

    let candidates = parse_candidates(&resp.content);
    let existing = ledger
        .active_memories_for_workdir(workdir)
        .await
        .unwrap_or_default();
    let existing_norm: Vec<String> = existing
        .iter()
        .map(|m| m.text.trim().to_lowercase())
        .collect();

    let mut inserted = Vec::new();
    for c in candidates.into_iter().take(MAX_CANDIDATES_PER_TASK) {
        let mut new = match sanitise(c, workdir) {
            Some(n) => n,
            None => continue,
        };
        if existing_norm
            .iter()
            .any(|e| similar_text(e, &new.text.to_lowercase()))
        {
            continue;
        }
        new.source_task_id = Some(task_id);
        match ledger.create_memory(new).await {
            Ok(rec) => inserted.push(rec.id),
            Err(e) => warn!(error = %e, "memory extractor: insert failed"),
        }
    }
    inserted
}

/// Crude near-duplicate check: identical when lowercased + whitespace
/// collapsed. Sufficient at the scale we're operating at (dozens of
/// memories per workdir, not millions).
fn similar_text(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_json_array() {
        let raw = r#"[{"kind":"fact","text":"Main branch is develop","scope":"workdir"}]"#;
        let v = parse_candidates(raw);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, "fact");
        assert_eq!(v[0].text, "Main branch is develop");
    }

    #[test]
    fn parses_with_markdown_fence() {
        let raw = "```json\n[{\"kind\":\"preference\",\"text\":\"Prefer English\"}]\n```";
        let v = parse_candidates(raw);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, "preference");
    }

    #[test]
    fn rejects_invalid_text_lengths() {
        let cand = LlmCandidate {
            kind: "fact".into(),
            text: "ok".into(), // too short
            scope: None,
        };
        assert!(sanitise(cand, "/x").is_none());
    }

    #[test]
    fn similar_text_is_whitespace_insensitive() {
        assert!(similar_text("Always run cargo check", "Always   run\tcargo check"));
        assert!(!similar_text("Always run cargo check", "Never run cargo check"));
    }
}
