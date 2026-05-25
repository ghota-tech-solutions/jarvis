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
    /// § T2.7 — optional Plan/Act/Verify/Ship phase tag emitted by the
    /// LLM to declare which lifecycle phase the next step belongs to.
    /// One of "plan" | "act" | "verify" | "ship" (case-insensitive,
    /// other strings are dropped silently). When unset, the previous
    /// phase carries over. v0 is observability only — sandbox/hook
    /// enforcement per phase lands in v1.
    pub phase: Option<Phase>,
}

/// § T2.7 — lifecycle phase declared by the agent. Used by the SPA's
/// future FSM swimlane view (F2.1) to bin events visually and to show
/// the user where the agent thinks it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Plan,
    Act,
    Verify,
    Ship,
}

impl Phase {
    #[allow(dead_code)] // v1 enforcement will read this; tests already exercise from_loose
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Act => "act",
            Self::Verify => "verify",
            Self::Ship => "ship",
        }
    }
    pub fn from_loose(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "plan" | "planning" => Some(Self::Plan),
            "act" | "acting" | "execute" | "executing" => Some(Self::Act),
            "verify" | "verifying" | "verification" | "test" | "testing" => Some(Self::Verify),
            "ship" | "shipping" | "deliver" | "delivering" | "deploy" => Some(Self::Ship),
            _ => None,
        }
    }
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
    /// § T2.7 — optional phase declaration. Loose-parsed so models can
    /// emit `"verify"`, `"verification"`, `"Verifying"`, etc.
    #[serde(default)]
    phase: Option<String>,
    /// Lenient: small models commonly hoist tool arguments to the top level
    /// of the reply instead of nesting them under `args` — Gemma 4 does this
    /// consistently for `update_plan` (`{"action":"update_plan","plan":[…]}`).
    /// Capture every unrecognized field so `normalize()` can fold them back
    /// into `args` for tool actions.
    #[serde(flatten)]
    extra: serde_json::Map<String, Json>,
}

/// Fold top-level "extra" fields into `args` when the model hoisted tool
/// arguments out of the `args` object. An explicit, non-empty `args` always
/// wins — extras are only used when `args` is absent, null, or `{}`.
fn recover_args(args: Option<Json>, extra: serde_json::Map<String, Json>) -> Option<Json> {
    let args_empty = match &args {
        None | Some(Json::Null) => true,
        Some(Json::Object(m)) => m.is_empty(),
        Some(_) => false,
    };
    if args_empty && !extra.is_empty() {
        Some(Json::Object(extra))
    } else {
        args
    }
}

impl RawReply {
    fn normalize(self) -> AgentReply {
        let phase = self.phase.as_deref().and_then(Phase::from_loose);
        let act_lower = self.action.to_ascii_lowercase();
        match act_lower.as_str() {
            "done" | "finish" | "complete" => AgentReply {
                thought: self.thought,
                action: ActionKind::Done,
                tool: None,
                args: None,
                message: self.message,
                phase,
            },
            "fail" | "give_up" | "abort" => AgentReply {
                thought: self.thought,
                action: ActionKind::Fail,
                tool: None,
                args: None,
                message: self.message,
                phase,
            },
            "tool" | "call_tool" | "use_tool" => AgentReply {
                thought: self.thought,
                action: ActionKind::Tool,
                tool: self.tool,
                args: recover_args(self.args, self.extra),
                message: self.message,
                phase,
            },
            // Anything else: assume the model put the tool name directly in `action`.
            tool_name => AgentReply {
                thought: self.thought,
                action: ActionKind::Tool,
                tool: self.tool.or_else(|| Some(tool_name.to_string())),
                args: recover_args(self.args, self.extra),
                message: self.message,
                phase,
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

/// § T1.8 — Hermes / Qwen / Mistral XML-wrapped JSON envelope.
///
/// The model emits:
/// ```text
/// <tool_call>{"name":"get_weather","arguments":{"location":"London"}}</tool_call>
/// ```
///
/// Variants accepted: `<tool_call>`, `<|tool_call|>` and trivial whitespace
/// drift around the JSON payload. Returns `None` when the envelope is absent
/// or when the inner payload is not valid JSON / does not carry `name`.
pub fn rewrite_hermes_xml(raw: &str) -> Option<String> {
    // Find an opening marker. We scan for `tool_call` then walk back/forward
    // for the surrounding angle bracket so we accept both `<tool_call>` and
    // `<|tool_call|>`.
    let (after_open_idx, _open_end) = find_hermes_open(raw)?;
    let rest = &raw[after_open_idx..];
    // Closing marker: `</tool_call>` or `<|/tool_call|>` or `<tool_call|>`.
    let close_rel = find_hermes_close(rest)?;
    let inner = rest[..close_rel].trim();
    // The inner payload is a JSON object — extract the first balanced one.
    let json_slice = extract_json(inner)?;
    let parsed: Json = serde_json::from_str(json_slice).ok()?;
    let obj = parsed.as_object()?;
    let name = obj.get("name").and_then(|v| v.as_str())?.trim();
    if name.is_empty() {
        return None;
    }
    let args = obj
        .get("arguments")
        .cloned()
        .or_else(|| obj.get("args").cloned())
        .unwrap_or_else(|| Json::Object(serde_json::Map::new()));
    let rewritten = serde_json::json!({
        "action": "call_tool",
        "tool": name,
        "args": args,
    });
    Some(rewritten.to_string())
}

/// Locate a Hermes opening tag (`<tool_call>` or `<|tool_call|>`).
/// Returns the byte index just past the closing `>` of the opener and the
/// byte index just past the opener itself (same value, exposed for symmetry).
fn find_hermes_open(raw: &str) -> Option<(usize, usize)> {
    let key = raw.find("tool_call")?;
    // Walk backwards to find the `<` that introduces the tag, accepting
    // up to a single `|` between `<` and `tool_call`.
    let before = &raw[..key];
    let lt = before.rfind('<')?;
    let between = &raw[lt + 1..key];
    if !between.is_empty() && between != "|" {
        return None;
    }
    // Walk forward from `key` to the closing `>`.
    let after = &raw[key + "tool_call".len()..];
    let gt_rel = after.find('>')?;
    let middle = &after[..gt_rel];
    // Accept `>`, `|>` ; reject any other inner chars (this would mean we
    // tripped over a closing tag instead).
    if !middle.is_empty() && middle != "|" {
        return None;
    }
    let end = key + "tool_call".len() + gt_rel + 1;
    Some((end, end))
}

/// Locate the closing Hermes tag and return its relative byte offset.
fn find_hermes_close(rest: &str) -> Option<usize> {
    // Look for `</tool_call>`, `<|/tool_call|>`, `<tool_call|>` (some
    // models drop the `/`), in that order of preference.
    let candidates = ["</tool_call>", "<|/tool_call|>", "<tool_call|>"];
    candidates.iter().filter_map(|tag| rest.find(tag)).min()
}

/// § T1.8 — Llama 3.1+ python-tag envelope.
///
/// The model emits:
/// ```text
/// <|python_tag|>fn_name(arg1="value", arg2=42, arg3=true)<|eom_id|>
/// ```
///
/// `<|eom_id|>` may be absent (some backends strip it as a stop token); we
/// accept that and read to end of string. Returns `None` when the opener is
/// missing or when the body does not parse as a call expression.
pub fn rewrite_llama_python(raw: &str) -> Option<String> {
    let start = raw.find("<|python_tag|>")? + "<|python_tag|>".len();
    let after = &raw[start..];
    let end = after
        .find("<|eom_id|>")
        .or_else(|| after.find("<|eot_id|>"))
        .unwrap_or(after.len());
    let body = after[..end].trim();
    parse_python_call(body).map(|v| v.to_string())
}

/// § T1.8 — generic ` ```tool_code ` / ` ```python ` fenced tool call.
///
/// `tool_code` is the preferred marker. `python` is accepted as a fallback
/// only when the fenced body actually looks like a function call (contains
/// `(` and `)`), to avoid swallowing real code samples.
pub fn rewrite_tool_code_block(raw: &str) -> Option<String> {
    // Prefer the `tool_code` marker; only fall back to `python` when present.
    let (body, _strict) = extract_fenced_body(raw, "tool_code")
        .map(|b| (b, true))
        .or_else(|| extract_fenced_body(raw, "python").map(|b| (b, false)))?;
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    // For `python` blocks (fallback), require call shape so we don't grab
    // an arbitrary script the model included as documentation.
    if !body.contains('(') || !body.contains(')') {
        return None;
    }
    parse_python_call(body).map(|v| v.to_string())
}

/// Extract the body of a ` ```<marker> ... ``` ` fenced block.
/// Returns `None` if no such block exists.
fn extract_fenced_body<'a>(raw: &'a str, marker: &str) -> Option<&'a str> {
    let needle_owned = format!("```{marker}");
    let start = raw.find(needle_owned.as_str())?;
    let after = &raw[start + needle_owned.len()..];
    // Skip an optional newline after the marker.
    let after = after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))
        .unwrap_or(after);
    let end = after.find("```")?;
    Some(&after[..end])
}

/// Parse a Python-style function call `fn(arg1="value", arg2=42, flag=true)`
/// into the JSON envelope `{"action":"call_tool","tool":fn,"args":{...}}`.
fn parse_python_call(body: &str) -> Option<Json> {
    let body = body.trim();
    let paren = body.find('(')?;
    let name = body[..paren].trim();
    if name.is_empty() || !is_ident(name) {
        return None;
    }
    // Find the matching closing paren — we expect the call to end the body.
    let after_open = &body[paren + 1..];
    let close = find_matching_paren(after_open)?;
    let args_src = &after_open[..close];

    let mut args = serde_json::Map::new();
    if !args_src.trim().is_empty() {
        for piece in split_python_args(args_src) {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            let eq = piece.find('=')?;
            let key = piece[..eq].trim();
            if key.is_empty() || !is_ident(key) {
                return None;
            }
            let value_src = piece[eq + 1..].trim();
            let value = parse_python_literal(value_src)?;
            args.insert(key.to_string(), value);
        }
    }

    Some(serde_json::json!({
        "action": "call_tool",
        "tool": name,
        "args": Json::Object(args),
    }))
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Return the byte index (within `s`) of the `)` that matches an implicit
/// opening `(` placed *before* `s`. Respects nested parens, square brackets,
/// curly braces, and string literals (single or double quotes, with backslash
/// escapes).
fn find_matching_paren(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    let mut quote: Option<u8> = None;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = quote {
            if escape {
                escape = false;
                continue;
            }
            match b {
                b'\\' => escape = true,
                x if x == q => quote = None,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' | b'\'' => quote = Some(b),
            b'(' => depth += 1,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            b'{' => brace += 1,
            b'}' => brace -= 1,
            b')' => {
                if depth == 0 && bracket == 0 && brace == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// Split a Python argument list on top-level commas, respecting quoted
/// strings and nested brackets / braces / parens.
fn split_python_args(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut depth = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    let mut quote: Option<u8> = None;
    let mut escape = false;
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = quote {
            if escape {
                escape = false;
                continue;
            }
            match b {
                b'\\' => escape = true,
                x if x == q => quote = None,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' | b'\'' => quote = Some(b),
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            b'{' => brace += 1,
            b'}' => brace -= 1,
            b',' if depth == 0 && bracket == 0 && brace == 0 => {
                out.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&body[start..]);
    out
}

/// Parse a single Python literal: quoted string, integer, float, bool, None.
/// Falls back to a JSON string when the value is not otherwise recognised.
fn parse_python_literal(s: &str) -> Option<Json> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // String literals: `"..."` or `'...'`. Support basic backslash escapes.
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        let inner = &s[1..s.len() - 1];
        return Some(Json::String(unescape_python_string(inner)));
    }
    if s.eq_ignore_ascii_case("true") {
        return Some(Json::Bool(true));
    }
    if s.eq_ignore_ascii_case("false") {
        return Some(Json::Bool(false));
    }
    if s == "None" || s.eq_ignore_ascii_case("null") {
        return Some(Json::Null);
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(Json::from(n));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Some(Json::from(f));
    }
    // JSON object / array literal — try to parse as-is.
    if ((s.starts_with('[') && s.ends_with(']')) || (s.starts_with('{') && s.ends_with('}')))
        && let Ok(v) = serde_json::from_str::<Json>(s)
    {
        return Some(v);
    }
    // Last resort: pass through as a string.
    Some(Json::String(s.to_string()))
}

fn unescape_python_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
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
        ToolDialect::HermesXml => match rewrite_hermes_xml(&dethought) {
            Some(rewritten) => std::borrow::Cow::Owned(rewritten),
            None => dethought,
        },
        ToolDialect::LlamaPython => match rewrite_llama_python(&dethought) {
            Some(rewritten) => std::borrow::Cow::Owned(rewritten),
            None => dethought,
        },
        ToolDialect::ToolCodeBlock => match rewrite_tool_code_block(&dethought) {
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
        phase: None,
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

    // Top-level args recovery — Gemma 4 hoists `plan` out of `args`.

    #[test]
    fn recovers_top_level_args_for_tool_name_action() {
        // Exact step-11 shape from task 29fa7542: `plan` at the top level,
        // `args` absent, tool name in `action`.
        let text = r#"{"thought":"t","action":"update_plan","plan":[{"step":"a","status":"completed"},{"step":"b","status":"in_progress"}]}"#;
        let r = parse_reply(text).unwrap();
        assert_eq!(r.action, ActionKind::Tool);
        assert_eq!(r.tool.as_deref(), Some("update_plan"));
        let plan = &r.args.as_ref().unwrap()["plan"];
        assert_eq!(plan.as_array().unwrap().len(), 2);
        assert_eq!(plan[0]["status"], "completed");
    }

    #[test]
    fn recovers_top_level_args_for_explicit_tool_action() {
        let text =
            r#"{"action":"tool","tool":"update_plan","plan":[{"step":"a","status":"pending"}]}"#;
        let r = parse_reply(text).unwrap();
        assert_eq!(r.action, ActionKind::Tool);
        assert_eq!(r.tool.as_deref(), Some("update_plan"));
        assert!(r.args.as_ref().unwrap()["plan"].is_array());
    }

    #[test]
    fn explicit_args_win_over_top_level_extras() {
        // A correct `args` is never overridden by stray top-level fields.
        let text = r#"{"action":"tool","tool":"shell","args":{"cmd":"ls"},"stray":"x"}"#;
        let r = parse_reply(text).unwrap();
        let args = r.args.as_ref().unwrap();
        assert_eq!(args["cmd"], "ls");
        assert!(args.get("stray").is_none());
    }

    #[test]
    fn empty_args_object_falls_back_to_extras() {
        let text = r#"{"action":"update_plan","args":{},"plan":[{"step":"a","status":"pending"}]}"#;
        let r = parse_reply(text).unwrap();
        assert!(r.args.as_ref().unwrap()["plan"].is_array());
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

    // § T1.8 — Hermes / Qwen XML tests.

    #[test]
    fn rewrite_hermes_xml_simple() {
        let raw =
            r#"<tool_call>{"name":"get_weather","arguments":{"location":"London"}}</tool_call>"#;
        let rewritten = rewrite_hermes_xml(raw).expect("should rewrite");
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["action"], "call_tool");
        assert_eq!(v["tool"], "get_weather");
        assert_eq!(v["args"]["location"], "London");
    }

    #[test]
    fn rewrite_hermes_xml_accepts_pipe_variant() {
        let raw =
            r#"<|tool_call|>{"name":"fs_read","arguments":{"path":"src/main.rs"}}<|/tool_call|>"#;
        let rewritten = rewrite_hermes_xml(raw).expect("should rewrite");
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["tool"], "fs_read");
        assert_eq!(v["args"]["path"], "src/main.rs");
    }

    #[test]
    fn rewrite_hermes_xml_returns_none_on_absent_envelope() {
        assert!(rewrite_hermes_xml(r#"{"action":"done"}"#).is_none());
        assert!(rewrite_hermes_xml("plain prose").is_none());
        assert!(rewrite_hermes_xml("").is_none());
    }

    #[test]
    fn rewrite_hermes_xml_returns_none_on_malformed_payload() {
        // Missing closing tag.
        assert!(rewrite_hermes_xml(r#"<tool_call>{"name":"foo"}"#).is_none());
        // Malformed JSON.
        assert!(rewrite_hermes_xml(r#"<tool_call>{not json}</tool_call>"#).is_none());
        // Missing name.
        assert!(rewrite_hermes_xml(r#"<tool_call>{"arguments":{}}</tool_call>"#).is_none());
        // Empty name.
        assert!(
            rewrite_hermes_xml(r#"<tool_call>{"name":"","arguments":{}}</tool_call>"#).is_none()
        );
    }

    #[test]
    fn preprocess_hermes_xml_round_trips_via_parse_reply() {
        let raw =
            r#"<tool_call>{"name":"grep","arguments":{"pattern":"TODO","path":"."}}</tool_call>"#;
        let cooked = preprocess_response(raw, jarvis_core::ToolDialect::HermesXml);
        let reply = parse_reply(&cooked).unwrap();
        assert_eq!(reply.action, ActionKind::Tool);
        assert_eq!(reply.tool.as_deref(), Some("grep"));
        let args = reply.args.as_ref().unwrap();
        assert_eq!(args["pattern"], "TODO");
        assert_eq!(args["path"], ".");
    }

    // § T1.8 — Llama python-tag tests.

    #[test]
    fn rewrite_llama_python_simple() {
        let raw = r#"<|python_tag|>get_weather(location="London", units="celsius")<|eom_id|>"#;
        let rewritten = rewrite_llama_python(raw).expect("should rewrite");
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["action"], "call_tool");
        assert_eq!(v["tool"], "get_weather");
        assert_eq!(v["args"]["location"], "London");
        assert_eq!(v["args"]["units"], "celsius");
    }

    #[test]
    fn rewrite_llama_python_handles_numbers_and_bools() {
        let raw =
            r#"<|python_tag|>fs_read(path="src/main.rs", start_line=42, follow=true)<|eom_id|>"#;
        let rewritten = rewrite_llama_python(raw).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["tool"], "fs_read");
        assert_eq!(v["args"]["path"], "src/main.rs");
        assert_eq!(v["args"]["start_line"], 42);
        assert_eq!(v["args"]["follow"], true);
    }

    #[test]
    fn rewrite_llama_python_accepts_missing_eom() {
        let raw = r#"<|python_tag|>shell(cmd="ls")"#;
        let rewritten = rewrite_llama_python(raw).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["tool"], "shell");
        assert_eq!(v["args"]["cmd"], "ls");
    }

    #[test]
    fn rewrite_llama_python_returns_none_on_garbage() {
        assert!(rewrite_llama_python("plain prose").is_none());
        assert!(rewrite_llama_python(r#"{"action":"done"}"#).is_none());
        assert!(rewrite_llama_python("").is_none());
        // Opener present but body has no parens.
        assert!(rewrite_llama_python("<|python_tag|>not a call<|eom_id|>").is_none());
    }

    #[test]
    fn preprocess_llama_python_round_trips_via_parse_reply() {
        let raw = r#"<|python_tag|>web_search(q="rust 2024 edition")<|eom_id|>"#;
        let cooked = preprocess_response(raw, jarvis_core::ToolDialect::LlamaPython);
        let reply = parse_reply(&cooked).unwrap();
        assert_eq!(reply.action, ActionKind::Tool);
        assert_eq!(reply.tool.as_deref(), Some("web_search"));
        assert_eq!(reply.args.as_ref().unwrap()["q"], "rust 2024 edition");
    }

    // § T1.8 — generic tool_code fenced block tests.

    #[test]
    fn rewrite_tool_code_block_simple() {
        let raw = "```tool_code\nget_weather(location=\"Paris\")\n```";
        let rewritten = rewrite_tool_code_block(raw).expect("should rewrite");
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["action"], "call_tool");
        assert_eq!(v["tool"], "get_weather");
        assert_eq!(v["args"]["location"], "Paris");
    }

    #[test]
    fn rewrite_tool_code_block_accepts_python_fallback() {
        let raw =
            "Sure, here's the call:\n```python\nfs_write(path=\"x.txt\", content=\"hi\")\n```\n";
        let rewritten = rewrite_tool_code_block(raw).expect("should rewrite");
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["tool"], "fs_write");
        assert_eq!(v["args"]["path"], "x.txt");
        assert_eq!(v["args"]["content"], "hi");
    }

    #[test]
    fn rewrite_tool_code_block_python_without_call_shape_returns_none() {
        // No parens — almost certainly real documentation, not a call.
        let raw = "```python\nx = 1\nprint(x)\n```";
        // This DOES contain parens (the print). Use a stricter example.
        let raw2 = "```python\nx = 1\ny = 2\n```";
        assert!(rewrite_tool_code_block(raw2).is_none());
        // The first one with parens at least gets attempted; it should fail
        // because `x = 1\nprint(x)` doesn't match `name(args)` shape.
        assert!(rewrite_tool_code_block(raw).is_none());
    }

    #[test]
    fn rewrite_tool_code_block_returns_none_on_absent_envelope() {
        assert!(rewrite_tool_code_block("just text").is_none());
        assert!(rewrite_tool_code_block(r#"{"action":"done"}"#).is_none());
        assert!(rewrite_tool_code_block("```\nfn()\n```").is_none());
    }

    #[test]
    fn rewrite_tool_code_block_prefers_tool_code_over_python() {
        let raw = "```tool_code\nfn_a(x=1)\n```\n```python\nfn_b(y=2)\n```";
        let rewritten = rewrite_tool_code_block(raw).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["tool"], "fn_a");
    }

    #[test]
    fn preprocess_tool_code_block_round_trips_via_parse_reply() {
        let raw = "```tool_code\ngrep(pattern=\"TODO\", path=\".\")\n```";
        let cooked = preprocess_response(raw, jarvis_core::ToolDialect::ToolCodeBlock);
        let reply = parse_reply(&cooked).unwrap();
        assert_eq!(reply.action, ActionKind::Tool);
        assert_eq!(reply.tool.as_deref(), Some("grep"));
        assert_eq!(reply.args.as_ref().unwrap()["pattern"], "TODO");
    }

    // § T1.8 — channel stripping still happens for all new dialects.

    #[test]
    fn preprocess_strips_think_channel_for_hermes() {
        let raw = "<|channel>thought\nreasoning<channel|><tool_call>{\"name\":\"shell\",\"arguments\":{\"cmd\":\"ls\"}}</tool_call>";
        let cooked = preprocess_response(raw, jarvis_core::ToolDialect::HermesXml);
        let reply = parse_reply(&cooked).unwrap();
        assert_eq!(reply.action, ActionKind::Tool);
        assert_eq!(reply.tool.as_deref(), Some("shell"));
    }

    // § T2.7 — phase declaration on the reply envelope.

    #[test]
    fn phase_field_parses_when_set() {
        let txt = r#"{"action":"tool","tool":"fs_read","args":{"path":"x"},"phase":"plan"}"#;
        let r = parse_reply(txt).unwrap();
        assert_eq!(r.phase, Some(Phase::Plan));
    }

    #[test]
    fn phase_loose_parses_common_variants() {
        for (raw, want) in [
            ("plan", Phase::Plan),
            ("planning", Phase::Plan),
            ("Act", Phase::Act),
            ("EXECUTING", Phase::Act),
            ("verify", Phase::Verify),
            ("verification", Phase::Verify),
            ("Testing", Phase::Verify),
            ("ship", Phase::Ship),
            ("deploy", Phase::Ship),
        ] {
            assert_eq!(Phase::from_loose(raw), Some(want), "raw was {raw:?}");
        }
    }

    #[test]
    fn phase_field_unknown_string_silently_drops() {
        let txt = r#"{"action":"done","message":"ok","phase":"wat"}"#;
        let r = parse_reply(txt).unwrap();
        assert_eq!(r.phase, None);
    }

    #[test]
    fn phase_absent_is_none() {
        let txt = r#"{"action":"done","message":"ok"}"#;
        let r = parse_reply(txt).unwrap();
        assert_eq!(r.phase, None);
    }

    #[test]
    fn phase_serializes_in_decision_payload() {
        // The agent loop writes the decision as
        // `serde_json::to_value(&reply)`; this asserts the field
        // round-trips so the SPA can read it back without backend
        // changes elsewhere.
        let txt =
            r#"{"action":"tool","tool":"shell","args":{"cmd":"cargo test"},"phase":"verify"}"#;
        let r = parse_reply(txt).unwrap();
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["phase"], "verify");
    }
}
