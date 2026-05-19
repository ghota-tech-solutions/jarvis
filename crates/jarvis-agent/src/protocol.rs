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

/// § C.M-C — rewrite a Gemma 4 native tool-call envelope into the JSON
/// contract the rest of the agent loop expects.
///
/// Gemma 4 was trained with six special tokens for tool use; when a
/// backend exposes them as text (Ollama `gemma4:*`, vLLM with
/// `--tool-call-parser gemma4` disabled, raw `mlx_lm.server` without
/// chat-template-managed tokens), a tool call looks like:
///
/// ```text
/// <|tool_call>call:get_weather{location:<|"|>London<|"|>}<tool_call|>
/// ```
///
/// This helper detects that pattern and rewrites it as
/// `{"action":"call_tool","tool":"get_weather","args":{"location":"London"}}`
/// so `parse_reply` can consume it unchanged. The rewriter is intentionally
/// conservative:
///
/// - If no envelope is found, returns the original text untouched.
/// - If the envelope is found but malformed (missing closing brace,
///   garbled args), returns the original text — the caller will fall
///   into the existing parse-failure recovery path (§ C.M-A).
/// - Only the FIRST envelope is rewritten. Multi-call replies are
///   uncommon on this path; if/when they appear we'll widen the
///   contract via a follow-up.
pub fn rewrite_gemma4_native(raw: &str) -> Option<String> {
    // Tolerant tag matching: Gemma 4 documents the tokens as
    // `<|tool_call>` ... `<tool_call|>`, but slightly different
    // variants show up in the wild (`<|tool_call|>...<|tool_call|>`,
    // backticks around the call name). We accept the load-bearing shape:
    //   <... tool_call ...> call:NAME { BODY } <... tool_call ...>
    let open = raw.find("tool_call")?;
    // Walk forward to the `call:` token after `tool_call`.
    let after_open = &raw[open + "tool_call".len()..];
    let call_pos = after_open.find("call:")?;
    let after_call = &after_open[call_pos + "call:".len()..];
    let brace = after_call.find('{')?;
    let name = after_call[..brace].trim().trim_matches(|c: char| {
        c == '`' || c == '"' || c == '\'' || c.is_whitespace() || c == '|' || c == '>'
    });
    if name.is_empty() {
        return None;
    }
    // Body terminates at the matching `}`. Gemma encloses string values
    // in `<|"|>...<|"|>`, so we strip those before parsing key=value.
    let body_src = &after_call[brace + 1..];
    let close = body_src.find('}')?;
    let body = &body_src[..close];

    let mut args = serde_json::Map::new();
    for piece in split_top_level_commas(body) {
        let (k, v) = piece.split_once(':')?;
        let key = k.trim().to_string();
        if key.is_empty() {
            return None;
        }
        let val = v.trim();
        // Strip the Gemma string-literal delimiters `<|"|>...<|"|>` if
        // present. Numeric / bool literals pass through unchanged.
        let stripped = val
            .strip_prefix("<|\"|>")
            .and_then(|s| s.strip_suffix("<|\"|>"))
            .map(|s| Json::String(s.to_string()))
            .unwrap_or_else(|| {
                // Try numeric / bool / null; fall back to string.
                if let Ok(n) = val.parse::<i64>() {
                    Json::from(n)
                } else if let Ok(f) = val.parse::<f64>() {
                    Json::from(f)
                } else if val.eq_ignore_ascii_case("true") {
                    Json::Bool(true)
                } else if val.eq_ignore_ascii_case("false") {
                    Json::Bool(false)
                } else if val.eq_ignore_ascii_case("null") {
                    Json::Null
                } else {
                    Json::String(val.trim_matches('"').to_string())
                }
            });
        args.insert(key, stripped);
    }

    let rewritten = serde_json::json!({
        "action": "call_tool",
        "tool": name,
        "args": Json::Object(args),
    });
    Some(rewritten.to_string())
}

/// Split a Gemma 4 args body on top-level commas, ignoring those inside
/// `<|"|>...<|"|>` string delimiters or nested braces. Returns &str
/// slices into the input.
fn split_top_level_commas(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        // Match `<|"|>` (5 bytes) at this position?
        if !in_string && bytes[i..].starts_with(br#"<|"|>"#) {
            in_string = true;
            i += 5;
            continue;
        }
        if in_string && bytes[i..].starts_with(br#"<|"|>"#) {
            in_string = false;
            i += 5;
            continue;
        }
        if !in_string {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                b',' if depth == 0 => {
                    out.push(&body[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    out.push(&body[start..]);
    out
}

/// § C thinking mode — strip Gemma 4's reflection channel.
///
/// When thinking mode is on, Gemma 4 emits its reasoning inside a
/// `<|channel>thought\n...<channel|>` block before the actual reply. The
/// model card also warns that "the model still generates empty tags on
/// most variants" even when thinking is disabled — so we strip the block
/// unconditionally before parsing. The reflection is reasoning, not the
/// JSON action; it must not reach `parse_reply` (and per the multi-turn
/// rule it must not enter history either).
///
/// Returns the input unchanged when no channel block is present.
pub fn strip_think_channel(raw: &str) -> Option<String> {
    if !raw.contains("<|channel") {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    let mut stripped_any = false;
    while let Some(start) = rest.find("<|channel") {
        // Everything before the channel opener stays.
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        // The block closes at `<channel|>` (or `<|channel|>` variant).
        // Search for the closing tag after the opener.
        let close_idx = after
            .match_indices("channel|>")
            .map(|(i, m)| i + m.len())
            .next();
        match close_idx {
            Some(end) => {
                rest = &after[end..];
                stripped_any = true;
            }
            None => {
                // Unterminated channel — drop the rest defensively.
                stripped_any = true;
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    if stripped_any {
        Some(out.trim().to_string())
    } else {
        None
    }
}

/// § C.M-C — dialect-aware preprocessing slot called before `parse_reply`.
/// Returns `Cow::Borrowed` for the common case (no rewrite needed).
pub fn preprocess_response(
    raw: &str,
    dialect: jarvis_core::ToolDialect,
) -> std::borrow::Cow<'_, str> {
    use jarvis_core::ToolDialect;
    // First, strip any Gemma reflection channel — applies regardless of
    // dialect (the model emits empty `<|channel>` tags even with thinking
    // off, and full blocks when it's on).
    let dethought: std::borrow::Cow<'_, str> = match strip_think_channel(raw) {
        Some(s) => std::borrow::Cow::Owned(s),
        None => std::borrow::Cow::Borrowed(raw),
    };
    match dialect {
        ToolDialect::Json | ToolDialect::Gemma4Strict => dethought,
        ToolDialect::Gemma4Native => match rewrite_gemma4_native(&dethought) {
            Some(rewritten) => std::borrow::Cow::Owned(rewritten),
            None => dethought,
        },
    }
}

/// § C.M-A — recovery for parse failures.
///
/// Some models (notably Gemma 4 on long contexts) drift out of the JSON
/// contract after several steps and reply with a freeform markdown summary
/// instead. The cheapest recovery that respects the model's intent: if the
/// reply contains no JSON-ish structure at all, treat it as an implicit
/// `Done` with the prose as the message. This avoids burning the rest of
/// the step budget on identical re-prompts.
///
/// Returns `Some(reply)` when the failure can be silently recovered, and
/// `None` when the caller should fall back to "inject a reminder and let
/// the model retry on the next step" (empty reply or malformed JSON
/// attempt — both signal the model is still trying to follow the contract).
pub fn recover_from_parse_failure(text: &str) -> Option<AgentReply> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Any opening or closing brace = the model was attempting JSON (even
    // if malformed or truncated). The caller will inject a reminder and
    // retry on the next step. Only fully brace-free prose gets coerced to
    // Done.
    if trimmed.contains('{') || trimmed.contains('}') {
        return None;
    }
    Some(AgentReply {
        thought: None,
        action: ActionKind::Done,
        tool: None,
        args: None,
        message: Some(trimmed.to_string()),
    })
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

    // § C.M-A — recovery tests.

    #[test]
    fn recovery_empty_text_returns_none() {
        assert!(recover_from_parse_failure("").is_none());
        assert!(recover_from_parse_failure("   \n\t  ").is_none());
    }

    #[test]
    fn recovery_pure_markdown_coerces_to_done() {
        // Mirrors the actual step-18 reply from task c867ee5d.
        let raw = "# Frontend Analysis Report\n\nI have performed a deep dive into the frontend of the Jarvis project, specifically focusing on the `jarvis-web` component.\n\n## Overview\nThe frontend is a modern SPA built with SolidJS.";
        let r = recover_from_parse_failure(raw).expect("should coerce to Done");
        assert_eq!(r.action, ActionKind::Done);
        assert!(r.message.unwrap().starts_with("# Frontend Analysis"));
        assert!(r.tool.is_none());
    }

    #[test]
    fn recovery_text_with_braces_returns_none() {
        // Looks like a malformed JSON attempt — the caller should reminder + retry.
        assert!(recover_from_parse_failure(r#"{"action": "tool", "tool":"shell""#).is_none());
        assert!(recover_from_parse_failure("Here is some text { with braces } in prose").is_none());
    }

    #[test]
    fn recovery_plain_sentence_coerces_to_done() {
        let r = recover_from_parse_failure("All done — no further changes needed.").unwrap();
        assert_eq!(r.action, ActionKind::Done);
        assert_eq!(
            r.message.as_deref(),
            Some("All done — no further changes needed.")
        );
    }

    // § C.M-C — Gemma4Native preprocessor tests.

    #[test]
    fn preprocess_passthrough_for_json_dialect() {
        let raw = r#"{"action":"done","message":"ok"}"#;
        let out = preprocess_response(raw, jarvis_core::ToolDialect::Json);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(out, raw);
    }

    #[test]
    fn preprocess_passthrough_for_gemma4_strict_dialect() {
        // Gemma4Strict still expects JSON from the model — preprocessor
        // does NOT rewrite even if the input looks like a native envelope.
        let raw = "<|tool_call>call:foo{x:1}<tool_call|>";
        let out = preprocess_response(raw, jarvis_core::ToolDialect::Gemma4Strict);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(out, raw);
    }

    #[test]
    fn rewrite_simple_gemma4_native_envelope() {
        let raw = r#"<|tool_call>call:get_weather{location:<|"|>London<|"|>}<tool_call|>"#;
        let rewritten = rewrite_gemma4_native(raw).expect("should rewrite");
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["action"], "call_tool");
        assert_eq!(v["tool"], "get_weather");
        assert_eq!(v["args"]["location"], "London");
    }

    #[test]
    fn rewrite_handles_numeric_and_string_args() {
        let raw =
            r#"<|tool_call>call:fs_read{path:<|"|>src/main.rs<|"|>,start_line:42}<tool_call|>"#;
        let rewritten = rewrite_gemma4_native(raw).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["tool"], "fs_read");
        assert_eq!(v["args"]["path"], "src/main.rs");
        assert_eq!(v["args"]["start_line"], 42);
    }

    #[test]
    fn rewrite_returns_none_when_no_envelope() {
        assert!(rewrite_gemma4_native(r#"{"action":"done"}"#).is_none());
        assert!(rewrite_gemma4_native("just some prose").is_none());
        assert!(rewrite_gemma4_native("").is_none());
    }

    #[test]
    fn rewrite_returns_none_on_malformed_envelope() {
        // No closing brace.
        assert!(rewrite_gemma4_native("<|tool_call>call:foo{x:1").is_none());
        // No call: prefix.
        assert!(rewrite_gemma4_native("<|tool_call>{x:1}<tool_call|>").is_none());
        // Empty name.
        assert!(rewrite_gemma4_native("<|tool_call>call:{x:1}<tool_call|>").is_none());
    }

    #[test]
    fn strip_think_channel_removes_block() {
        let raw = "<|channel>thought\nLet me reason about this carefully.<channel|>{\"action\":\"done\",\"message\":\"ok\"}";
        let out = strip_think_channel(raw).expect("should strip");
        assert_eq!(out, r#"{"action":"done","message":"ok"}"#);
    }

    #[test]
    fn strip_think_channel_none_when_absent() {
        assert!(strip_think_channel(r#"{"action":"done"}"#).is_none());
        assert!(strip_think_channel("plain text").is_none());
    }

    #[test]
    fn strip_think_channel_handles_empty_tags() {
        // The card warns empty tags appear even with thinking disabled.
        let raw = "<|channel>thought\n<channel|>{\"action\":\"done\"}";
        let out = strip_think_channel(raw).unwrap();
        assert_eq!(out, r#"{"action":"done"}"#);
    }

    #[test]
    fn strip_think_channel_unterminated_drops_tail() {
        let raw = "{\"action\":\"tool\"}<|channel>thought never closed";
        let out = strip_think_channel(raw).unwrap();
        assert_eq!(out, r#"{"action":"tool"}"#);
    }

    #[test]
    fn preprocess_strips_think_channel_for_json_dialect() {
        let raw =
            "<|channel>thought\nreasoning here<channel|>{\"action\":\"done\",\"message\":\"hi\"}";
        let out = preprocess_response(raw, jarvis_core::ToolDialect::Json);
        let reply = parse_reply(&out).unwrap();
        assert_eq!(reply.action, ActionKind::Done);
        assert_eq!(reply.message.as_deref(), Some("hi"));
    }

    #[test]
    fn preprocess_native_envelope_round_trips_via_parse_reply() {
        // End-to-end smoke: rewrite then parse_reply produces a usable
        // AgentReply with the right tool name and args.
        let raw = r#"<|tool_call>call:web_search{q:<|"|>tonic 0.13<|"|>}<tool_call|>"#;
        let cooked = preprocess_response(raw, jarvis_core::ToolDialect::Gemma4Native);
        let reply = parse_reply(&cooked).unwrap();
        assert_eq!(reply.action, ActionKind::Tool);
        assert_eq!(reply.tool.as_deref(), Some("web_search"));
        assert_eq!(
            reply.args.as_ref().unwrap()["q"],
            serde_json::Value::String("tonic 0.13".to_string())
        );
    }
}
