//! Parsing the LLM's reply into structured agent actions.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// The LLM is asked to reply with one JSON object per turn:
/// ```json
/// {
///   "thought": "short reasoning",
///   "action": "tool" | "done" | "fail",
///   "tool":   "shell" | "fs_read" | "fs_write",   // when action == "tool"
///   "args":   { ... },                            // when action == "tool"
///   "message": "..."                              // when action == "done" | "fail"
/// }
/// ```
/// Lenient: smaller models commonly write `"action": "<tool_name>"` directly.
/// We normalize that to `action=tool, tool=<tool_name>` at parse time.
#[derive(Debug, Clone, Serialize)]
pub struct AgentReply {
    pub thought: Option<String>,
    pub action: ActionKind,
    pub tool: Option<String>,
    pub args: Option<Json>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Tool,
    Done,
    Fail,
}

/// Raw shape exactly as the LLM may emit it (action is free-form).
#[derive(Debug, Clone, Deserialize)]
struct RawReply {
    #[serde(default)]
    thought: Option<String>,
    action: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    args: Option<Json>,
    #[serde(default)]
    message: Option<String>,
}

impl RawReply {
    fn normalize(self) -> AgentReply {
        let act_lower = self.action.to_ascii_lowercase();
        match act_lower.as_str() {
            "done" | "finish" | "complete" => AgentReply {
                thought: self.thought,
                action: ActionKind::Done,
                tool: None,
                args: None,
                message: self.message,
            },
            "fail" | "give_up" | "abort" => AgentReply {
                thought: self.thought,
                action: ActionKind::Fail,
                tool: None,
                args: None,
                message: self.message,
            },
            "tool" | "call_tool" | "use_tool" => AgentReply {
                thought: self.thought,
                action: ActionKind::Tool,
                tool: self.tool,
                args: self.args,
                message: self.message,
            },
            // Anything else: assume the model put the tool name directly in `action`.
            tool_name => AgentReply {
                thought: self.thought,
                action: ActionKind::Tool,
                tool: self.tool.or_else(|| Some(tool_name.to_string())),
                args: self.args,
                message: self.message,
            },
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("provider: {0}")]
    Provider(#[from] jarvis_core::Error),
    #[error("ledger: {0}")]
    Ledger(#[from] jarvis_ledger::LedgerError),
    #[error("tool: {0}")]
    Tool(#[from] jarvis_tools::ToolError),
    #[error("LLM reply could not be parsed as JSON: {0}")]
    ParseReply(String),
    #[error("LLM reply missing field: {0}")]
    MissingField(&'static str),
    #[error("budget exhausted ({0} steps)")]
    BudgetExhausted(u32),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Agent declared the goal achieved.
    Done,
    /// Agent declared failure.
    Failed,
    /// Loop terminated for external reasons (budget, cancel).
    Aborted,
}

/// Extract the first balanced JSON object from `text`, ignoring anything outside.
/// Supports ```json fences and inline objects.
pub fn extract_json(text: &str) -> Option<&str> {
    // Strip optional ```json … ``` fences.
    if let Some(start) = text.find("```json") {
        let after = &text[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if !candidate.is_empty() {
                return Some(candidate);
            }
        }
    }
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if candidate.starts_with('{') {
                return Some(candidate);
            }
        }
    }
    // Fallback: find the first '{' and walk to the matching '}'.
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escape {
                escape = false;
                continue;
            }
            match b {
                b'\\' => escape = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn parse_reply(text: &str) -> Result<AgentReply, AgentError> {
    let snippet = extract_json(text)
        .ok_or_else(|| AgentError::ParseReply(format!("no JSON object found in:\n{text}")))?;
    let raw: RawReply = serde_json::from_str(snippet)
        .map_err(|e| AgentError::ParseReply(format!("{e}\nsnippet:\n{snippet}")))?;
    Ok(raw.normalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_object() {
        let r = parse_reply(r#"{"thought":"hi","action":"done","message":"ok"}"#).unwrap();
        assert_eq!(r.action, ActionKind::Done);
        assert_eq!(r.message.as_deref(), Some("ok"));
    }

    #[test]
    fn parses_fenced_json() {
        let text = "Some preamble.\n```json\n{\"thought\":\"t\",\"action\":\"tool\",\"tool\":\"shell\",\"args\":{\"cmd\":\"ls\"}}\n```\ntrailing words";
        let r = parse_reply(text).unwrap();
        assert_eq!(r.action, ActionKind::Tool);
        assert_eq!(r.tool.as_deref(), Some("shell"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_reply("no json here at all").is_err());
    }

    #[test]
    fn normalizes_tool_name_as_action() {
        // Gemma 4 26B commonly emits this shape.
        let text = r#"{"thought":"t","action":"fs_write","tool":"fs_write","args":{"path":"x","content":"y"}}"#;
        let r = parse_reply(text).unwrap();
        assert_eq!(r.action, ActionKind::Tool);
        assert_eq!(r.tool.as_deref(), Some("fs_write"));
    }

    #[test]
    fn normalizes_done_synonyms() {
        let text = r#"{"action":"finish","message":"ok"}"#;
        let r = parse_reply(text).unwrap();
        assert_eq!(r.action, ActionKind::Done);
    }
}
