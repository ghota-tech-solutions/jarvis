//! Verdict-gate validator — second-LLM "is this really done?" check.
//!
//! Called by the agent loop when the model emits `action=done`. If the
//! validator agrees (`ACK`), the verdict goes through unchanged. If it
//! disagrees (`NACK`), the loop receives a synthetic Continuation event
//! describing why and gets another turn to actually finish the work.
//!
//! Cost-aware:
//! - Only runs when `cfg.validation.enabled == true`.
//! - Caps total validator-driven continuations per task at
//!   `cfg.validation.max_validations` (default 1) so a flaky validator
//!   can't loop forever.
//! - Calls the validator with `max_tokens = 240` and a tightly-templated
//!   prompt — typical cost is ~$0.001 per call on remote models.

use jarvis_core::{ChatMessage, ChatRequest, LlmProvider};
use std::sync::Arc;
use tracing::warn;

/// Outcome of one validator pass.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// Validator endorsed the agent's done. Let it through.
    Ack,
    /// Validator rejected. `reason` is one line, suitable for injection as
    /// a `Continuation` event payload.
    Nack { reason: String },
    /// Validator was unreachable / its reply was unparseable. Treat as Ack
    /// to avoid blocking the agent on transient infra.
    Skip { reason: String },
}

const VALIDATOR_SYSTEM: &str = "You are a strict verdict validator. Another \
    coding agent has emitted `done`. Your job: decide if its actions \
    actually accomplish the user's goal, based on the evidence given.\n\n\
    Reply with EXACTLY ONE LINE in one of these shapes:\n\
      ACK\n\
      NACK: <one-line reason, < 200 chars>\n\n\
    Use NACK when:\n\
    - The goal is multi-step and the agent stopped early.\n\
    - The agent wrote code that obviously won't compile / will crash.\n\
    - A post-tool hook (e.g. cargo check) failed and was ignored.\n\
    - The agent's `message` says it succeeded but the actual artifacts show otherwise.\n\
    Use ACK when:\n\
    - The goal is plausibly accomplished given the evidence.\n\
    - The goal is informational and the agent's message answers it directly.\n\
    Do NOT add any commentary. Single line, ACK or NACK: …";

/// Build the validator prompt from the task's goal + the agent's done
/// message + a tail of recent relevant events for context.
pub fn build_request(
    goal: &str,
    done_message: Option<&str>,
    recent: &[jarvis_ledger::EventRecord],
) -> ChatRequest {
    let mut evidence = String::with_capacity(2048);
    evidence.push_str(&format!("Goal: {goal}\n\n"));
    if let Some(m) = done_message {
        evidence.push_str(&format!("Agent's done message:\n{m}\n\n"));
    }
    evidence.push_str("Recent agent actions (oldest → newest):\n");
    for e in recent.iter().take(20) {
        let pay = serde_json::to_string(&e.payload).unwrap_or_default();
        let pay = if pay.len() > 240 {
            format!("{}…", &pay[..240])
        } else {
            pay
        };
        evidence.push_str(&format!("- {}: {pay}\n", e.kind.as_str()));
    }

    ChatRequest {
        messages: vec![
            ChatMessage::system(VALIDATOR_SYSTEM.to_string()),
            ChatMessage::user(evidence),
        ],
        temperature: Some(0.0),
        max_tokens: Some(240),
        stream: false,
    }
}

/// Parse the validator's single-line reply.
pub fn parse_reply(raw: &str) -> Verdict {
    let line = raw.lines().next().unwrap_or("").trim();
    if line.eq_ignore_ascii_case("ACK") {
        return Verdict::Ack;
    }
    if let Some(rest) = line
        .strip_prefix("NACK:")
        .or_else(|| line.strip_prefix("NACK"))
    {
        let reason = rest.trim_start_matches([':', ' ']).trim().to_string();
        let reason = if reason.is_empty() {
            "validator said NACK without a reason".to_string()
        } else {
            reason
        };
        return Verdict::Nack { reason };
    }
    Verdict::Skip {
        reason: format!(
            "unparseable validator reply: {}",
            &line.chars().take(80).collect::<String>()
        ),
    }
}

/// Run one validator pass. Errors are swallowed into `Verdict::Skip` so
/// the agent loop never blocks on infra trouble.
pub async fn validate(
    provider: Arc<dyn LlmProvider>,
    goal: &str,
    done_message: Option<&str>,
    recent: &[jarvis_ledger::EventRecord],
) -> Verdict {
    let req = build_request(goal, done_message, recent);
    match provider.complete(req).await {
        Ok(reply) => parse_reply(&reply.content),
        Err(e) => {
            warn!(error = %e, "validator call failed; skipping (treating as ACK)");
            Verdict::Skip {
                reason: format!("provider error: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ack() {
        assert!(matches!(parse_reply("ACK"), Verdict::Ack));
        assert!(matches!(parse_reply("ack"), Verdict::Ack));
        assert!(matches!(
            parse_reply("ACK\nignored second line"),
            Verdict::Ack
        ));
    }

    #[test]
    fn parse_nack_with_reason() {
        match parse_reply("NACK: tests are red") {
            Verdict::Nack { reason } => assert_eq!(reason, "tests are red"),
            _ => panic!("expected NACK"),
        }
        // Tolerate missing space + colon.
        match parse_reply("NACK code didn't compile") {
            Verdict::Nack { reason } => assert_eq!(reason, "code didn't compile"),
            _ => panic!("expected NACK"),
        }
    }

    #[test]
    fn parse_bare_nack_uses_default_reason() {
        match parse_reply("NACK") {
            Verdict::Nack { reason } => assert!(reason.contains("without a reason")),
            _ => panic!("expected NACK"),
        }
    }

    #[test]
    fn unparseable_becomes_skip() {
        match parse_reply("hmm I'm not sure") {
            Verdict::Skip { reason } => assert!(reason.starts_with("unparseable")),
            _ => panic!("expected Skip"),
        }
    }
}
