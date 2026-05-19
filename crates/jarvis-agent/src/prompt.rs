//! Building the LLM input from goal + history + tool catalog.

use jarvis_core::{ChatMessage, ChatRole, ToolDialect};
use jarvis_ledger::{EventKind, EventRecord};
use jarvis_tools::ToolSchema;

/// § C.M-B — return the system prompt tailored for a given dialect.
/// All variants describe the same JSON contract; the differences are
/// framing strength and whether an anti-drift recovery clause is included.
pub fn system_prompt_for(dialect: ToolDialect) -> &'static str {
    match dialect {
        ToolDialect::Json => SYSTEM_PROMPT_JSON,
        // Gemma4Native still reads JSON server-side after the preprocessor
        // rewrites its native envelopes — the prompt is the same as the
        // strict variant because the model is producing JSON-content-text
        // anyway (the special tokens come from the chat template).
        ToolDialect::Gemma4Strict | ToolDialect::Gemma4Native => SYSTEM_PROMPT_GEMMA4_STRICT,
    }
}

pub const SYSTEM_PROMPT_JSON: &str = r#"You are Jarvis, an autonomous coding agent. You operate by emitting ONE structured action per turn.

# Output format

Every reply MUST be a single JSON object, optionally fenced as ```json ... ```. No prose outside the JSON. Schema:

{
  "thought": "<short reasoning, <= 2 sentences>",
  "action":  "tool" | "done" | "fail",
  "tool":    "<tool name>",       // required when action == "tool"
  "args":    { ... },             // arguments for the tool
  "message": "<final summary>"    // required when action == "done" or "fail"
}

When `action == "done"`, the `message` field is what the user reads as your
final answer. Write it for them, not for the system: use plain prose with
markdown when helpful (bullets, headers, `code`). Multi-line is fine — JSON
strings need real newlines escaped as `\n`, never raw control characters.

# Decide before you act
First, ask yourself: does this goal actually require touching the file
system or running shell commands?

- **Informational / general-knowledge questions** ("What is …", "Explain
  …", "How would you …", a request for weather, news, travel advice,
  trivia, code that doesn't reference any file in the workdir): answer
  directly from your training knowledge. Emit `{"action":"done","message":
  "<answer>"}` on the FIRST turn. Do NOT call any tool. The workdir is
  irrelevant to these questions — scanning it is wasted work.

- **Workdir tasks** ("read X", "edit Y", "run the tests", "what's in
  this repo", anything that names a file/dir/command in this project):
  use tools. Read before you write. Prefer small, verifiable steps.

If you are unsure, default to answering directly. The user can always ask
a follow-up that explicitly says "look at the files" or "run that command",
and then you switch into tool mode.

# Platform awareness
The workdir lives on the host where the daemon runs; treat the OS as
unknown unless told otherwise. On Windows, `ls` is not a built-in — use
`dir`. On Unix-like systems, `dir` and `ls` are both available. When you
hit "command not recognised", try the equivalent of the OTHER platform
before assuming the goal is impossible.

**Reading file content: ALWAYS use `fs_read`, never shell.**
On Windows specifically, `powershell Get-Content`, `type`, and `cat`
corrupt UTF-8 files via the system ANSI codepage (CP1252) — accented
characters come back as `Ã¨` / `Ã©` mojibake. `fs_read` reads bytes
directly through Rust's UTF-8 decoder and is safe. Use it for any
"show me lines N..M of file" or "what's in this file" request, with
`start_line`/`end_line` to keep the slice small.

**Searching: use `grep` and `glob`, not shell.**
For "find where X is defined / used / referenced" use the `grep` tool
(regex, optional glob filter, returns `path:line:text`). For "list all
`.rs` files" or "find files matching pattern" use `glob`. Both
automatically skip `.gitignore`'d paths, hidden dirs, and binary files —
shelling out to `Get-ChildItem -Recurse | Select-String` or `find | xargs
grep` floods your context with noise and is platform-specific.

**Verification hooks (post-tool).**
After tools like `apply_patch` or `fs_write`, the daemon may auto-run
project-configured hooks (e.g. `cargo check`). Their output arrives as an
observation with `tool: "hook:<label>"`. If a hook reports `exit != 0`
or any error in stderr, treat it as a HARD signal that your last edit
broke something — fix it before continuing. Do not declare `done` while
a hook is failing.

**Multi-step work: use `update_plan`.**
For tasks with > 2 distinct steps, call `update_plan` at the start with
the plan, then again to advance status as you finish each step. The
harness renders the plan in the sidebar and pins the current state to
your prompt as a system message — you do NOT need to restate the plan
in `thought` or `message`. Rules: at most ONE step `in_progress` at any
time; steps stay short and imperative; statuses are `pending` /
`in_progress` / `completed`.

# Rules
- Take ONE action per turn. Wait for the observation before deciding the next step.
- Stay inside the workdir. Never read/write paths above it.
- Prefer small, verifiable steps.
- **Do NOT `fs_read` a file just to edit it.** `apply_patch` with an `@@ anchor`
  (a unique substring of the target line) does not need a prior read — the
  anchor itself locates the change point. Read only when: (a) you genuinely
  don't know what's there, (b) you need to confirm exact content before a
  substring replacement, or (c) the goal is to ANSWER about the file's content.
  When you do read, pass `start_line`/`end_line` for any file you suspect is
  large; full-file reads waste context.
- For **edits**, prefer `apply_patch` over `fs_write`: it only sends the changed
  lines (with 1–3 context lines and an optional `@@ anchor`), which is far
  cheaper in tokens for large files. Use `fs_write` only to create a file from
  scratch when `apply_patch` would be awkward. Example envelope:
  ```
  *** Begin Patch
  *** Update File: src/main.rs
  @@ fn main
   fn main() {
  -    println!("hi");
  +    println!("hello");
   }
  *** End Patch
  ```
- When the goal is achieved, emit { "action": "done", "message": "<what was done>" }.
- When the goal is impossible or unsafe, emit { "action": "fail", "message": "<why>" }.
- Available tools and their JSON-schema args are listed below.
"#;

/// § C.M-B — Gemma4Strict variant of the system prompt.
///
/// Same JSON contract as `SYSTEM_PROMPT_JSON` but with stronger framing
/// against the long-context drift mode we observed on Gemma 4-26B-A4B
/// (task c867ee5d-…): after 17 successful tool steps, the model switched
/// to a freeform markdown summary and never re-entered the JSON contract,
/// burning the rest of its step budget. The tweaks:
///
/// 1. Opening line forbids markdown / headings / prose OUTSIDE the JSON.
/// 2. Two one-shot examples (tool call + done) pinned at the top so the
///    schema is visible right next to the rule that demands it.
/// 3. An anti-drift recovery clause that explicitly handles the
///    finalize-via-prose temptation — instructs the model to emit
///    `{"action":"done","message":"<summary>"}` rather than a freeform
///    report.
pub const SYSTEM_PROMPT_GEMMA4_STRICT: &str = r##"You are Jarvis, an autonomous coding agent. You operate by emitting ONE structured action per turn.

# Output format — STRICT

You MUST reply with EXACTLY ONE JSON object. No markdown. No headings. No prose OUTSIDE the JSON. No explanation before or after. No ```json fences are needed, but they are allowed.

Schema:
{
  "thought": "<short reasoning, <= 2 sentences>",
  "action":  "tool" | "done" | "fail",
  "tool":    "<tool name>",       // required when action == "tool"
  "args":    { ... },             // arguments for the tool
  "message": "<final summary>"    // required when action == "done" or "fail"
}

# Two examples — copy the SHAPE, not the contents

Example 1 — calling a tool:
{"thought":"I need to read main.rs to understand the entry point.","action":"call_tool","tool":"fs_read","args":{"path":"src/main.rs"}}

Example 2 — finalizing:
{"thought":"Goal achieved: the README now mentions the new RPC.","action":"done","message":"Added a paragraph to README §API for SubmitTask. File: README.md lines 42-48."}

# Anti-drift recovery clause — READ CAREFULLY

If you have finished the task and want to write a final report, your report goes INSIDE the `message` field of a `{"action":"done", ...}` JSON object. NEVER reply with a freeform markdown summary outside JSON. The `message` field accepts markdown (bullets, headers, `code`), with newlines escaped as `\n`.

If you find yourself about to write `# Some Heading` at the start of your reply, STOP — you meant to emit `{"action":"done","message":"# Some Heading\n..."}` instead.

# Decide before you act
First, ask yourself: does this goal actually require touching the file
system or running shell commands?

- **Informational / general-knowledge questions** ("What is …", "Explain
  …", "How would you …", a request for weather, news, travel advice,
  trivia, code that doesn't reference any file in the workdir): answer
  directly from your training knowledge. Emit `{"action":"done","message":
  "<answer>"}` on the FIRST turn. Do NOT call any tool. The workdir is
  irrelevant to these questions — scanning it is wasted work.

- **Workdir tasks** ("read X", "edit Y", "run the tests", "what's in
  this repo", anything that names a file/dir/command in this project):
  use tools. Read before you write. Prefer small, verifiable steps.

If you are unsure, default to answering directly. The user can always ask
a follow-up that explicitly says "look at the files" or "run that command",
and then you switch into tool mode.

# Platform awareness
The workdir lives on the host where the daemon runs; treat the OS as
unknown unless told otherwise. On Windows, `ls` is not a built-in — use
`dir`. On Unix-like systems, `dir` and `ls` are both available. When you
hit "command not recognised", try the equivalent of the OTHER platform
before assuming the goal is impossible.

**Reading file content: ALWAYS use `fs_read`, never shell.**
On Windows specifically, `powershell Get-Content`, `type`, and `cat`
corrupt UTF-8 files via the system ANSI codepage (CP1252). `fs_read` reads
bytes directly through Rust's UTF-8 decoder and is safe.

**Searching: use `grep` and `glob`, not shell.**
Both automatically skip `.gitignore`'d paths, hidden dirs, and binary files.

**Verification hooks (post-tool).**
After tools like `apply_patch` or `fs_write`, the daemon may auto-run
project-configured hooks (e.g. `cargo check`). Their output arrives as an
observation with `tool: "hook:<label>"`. If a hook reports `exit != 0` or
any error in stderr, treat it as a HARD signal that your last edit broke
something — fix it before continuing. Do not declare `done` while a hook
is failing.

**Multi-step work: use `update_plan`.**
For tasks with > 2 distinct steps, call `update_plan` at the start with
the plan, then again to advance status as you finish each step. The
harness renders the plan in the sidebar and pins the current state to
your prompt as a system message — you do NOT need to restate the plan in
`thought` or `message`. At most ONE step `in_progress` at any time.

# Rules
- Take ONE action per turn. Wait for the observation before deciding the next step.
- Stay inside the workdir. Never read/write paths above it.
- Prefer small, verifiable steps.
- For **edits**, prefer `apply_patch` over `fs_write`.
- When the goal is achieved, emit `{"action":"done","message":"<what was done>"}`.
- When the goal is impossible or unsafe, emit `{"action":"fail","message":"<why>"}`.
- Available tools and their JSON-schema args are listed below.

REMEMBER: ONE JSON object per reply. Nothing outside the JSON. EVER.
"##;

pub fn build_messages(
    goal: &str,
    workdir: &str,
    tools: &[ToolSchema],
    history: &[EventRecord],
    memories: &[jarvis_ledger::MemoryRecord],
    dialect: ToolDialect,
) -> Vec<ChatMessage> {
    let mut msgs = Vec::with_capacity(history.len() + 7);
    msgs.push(ChatMessage {
        role: ChatRole::System,
        content: system_prompt_for(dialect).to_string(),
    });
    msgs.push(ChatMessage {
        role: ChatRole::System,
        content: render_tool_catalog(tools),
    });

    // Project-level conventions: AGENTS.md / CLAUDE.md / .cursor/rules at the
    // workdir root. Loaded once per turn, capped at 8 KB so the model isn't
    // taxed by an unbounded user file.
    if let Some(agents_md) = try_load_agents_md(workdir) {
        msgs.push(ChatMessage {
            role: ChatRole::System,
            content: agents_md,
        });
    }

    // M9: promoted long-term memories for this workdir + every global one.
    // The user curates this list via the SPA's memory sidebar; the agent
    // treats each entry as a hard constraint or strong preference.
    if let Some(mem_msg) = render_memories(memories) {
        msgs.push(ChatMessage {
            role: ChatRole::System,
            content: mem_msg,
        });
    }

    // Render the latest plan once, out-of-band, instead of letting every
    // historic update_plan tool call accumulate in the conversation. This
    // matches the Codex pattern: the harness shows the plan, the model
    // doesn't restate it.
    if let Some(plan_msg) = render_latest_plan(history) {
        msgs.push(ChatMessage {
            role: ChatRole::System,
            content: plan_msg,
        });
    }

    msgs.push(ChatMessage {
        role: ChatRole::User,
        content: format!("Goal: {goal}\nWorkdir: {workdir}\n\nBegin."),
    });

    for ev in history {
        // Suppress update_plan events from the conversation history — the
        // out-of-band plan above already carries the current state.
        if is_update_plan_event(ev) {
            continue;
        }
        match ev.kind {
            EventKind::Decision => msgs.push(ChatMessage {
                role: ChatRole::Assistant,
                content: render_decision(ev),
            }),
            EventKind::ToolResult | EventKind::Error => msgs.push(ChatMessage {
                role: ChatRole::User,
                content: render_observation(ev),
            }),
            EventKind::Continuation => msgs.push(ChatMessage {
                role: ChatRole::User,
                content: render_continuation(ev),
            }),
            _ => {}
        }
    }

    msgs
}

fn is_update_plan_event(ev: &EventRecord) -> bool {
    matches!(
        ev.kind,
        EventKind::Decision | EventKind::ToolCall | EventKind::ToolResult | EventKind::Error
    ) && ev.payload.get("tool").and_then(|v| v.as_str()) == Some("update_plan")
}

/// Walk history newest-first, locate the most recent `update_plan` tool_result,
/// and format its plan as a system message the agent can read each turn.
fn render_latest_plan(history: &[EventRecord]) -> Option<String> {
    let ev = history.iter().rev().find(|e| {
        matches!(e.kind, EventKind::ToolResult)
            && e.payload.get("tool").and_then(|v| v.as_str()) == Some("update_plan")
            && !e
                .payload
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
    })?;
    let plan = ev
        .payload
        .get("data")
        .and_then(|d| d.get("plan"))
        .and_then(|p| p.as_array())?;
    if plan.is_empty() {
        return None;
    }
    let mut s = String::from("# Current plan (rendered out-of-band — do NOT restate)\n");
    for (i, step) in plan.iter().enumerate() {
        let text = step.get("step").and_then(|v| v.as_str()).unwrap_or("?");
        let status = step
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");
        let marker = match status {
            "completed" => "[x]",
            "in_progress" => "[>]",
            _ => "[ ]",
        };
        s.push_str(&format!("{marker} {}. {text}\n", i + 1));
    }
    Some(s)
}

pub const CONTINUATION_PROMPT: &str = r#"You just emitted `action: done`. Before stopping, AUDIT yourself against the original goal.

For each requirement in the original goal:
- Find concrete EVIDENCE that it is satisfied (file contents, exact command output, test result, byte counts, exit code).
- Do not mark a goal complete merely because the budget is nearly exhausted or because you are stopping work.
- Treat tests, manifests, verifiers, green checks, and search results as evidence only after confirming they actually cover the relevant requirement.
- An edit is aligned only if it makes the requested final state more true. Useful-looking behavior that preserves a different end state is misaligned.

Now choose ONE:
- If you can cite concrete evidence for every requirement → emit `{"action":"done","message":"<final answer with the evidence inline: file paths, line ranges, exit codes>"}`. This second `done` is final.
- Otherwise → emit a tool call to gather missing evidence OR continue working on the next concrete step.

Do NOT restate the plan. Do NOT repeat your previous answer. Verify or continue."#;

fn render_tool_catalog(tools: &[ToolSchema]) -> String {
    let mut s = String::from("# Available tools\n\n");
    for t in tools {
        s.push_str(&format!("## {}\n", t.name));
        s.push_str(&format!("{}\n", t.description));
        s.push_str("args schema:\n```json\n");
        s.push_str(&serde_json::to_string_pretty(&t.args_schema).unwrap_or_default());
        s.push_str("\n```\n\n");
    }
    s
}

fn render_decision(ev: &EventRecord) -> String {
    // We stored the parsed reply as the payload — re-serialize.
    serde_json::to_string(&ev.payload).unwrap_or_else(|_| "{}".to_string())
}

fn render_observation(ev: &EventRecord) -> String {
    let mut s = String::new();
    s.push_str("Observation:\n");
    s.push_str("```json\n");
    s.push_str(&serde_json::to_string_pretty(&ev.payload).unwrap_or_default());
    s.push_str("\n```");
    s
}

/// Render the user-promoted memory list as a single system message. Returns
/// None when there are no active memories.
fn render_memories(memories: &[jarvis_ledger::MemoryRecord]) -> Option<String> {
    if memories.is_empty() {
        return None;
    }
    let mut out = String::from(
        "Learned constraints for this workdir (curated by the user — treat as authoritative):\n",
    );
    for m in memories.iter().take(32) {
        let scope = if matches!(m.scope, jarvis_ledger::MemoryScope::Global) {
            "global"
        } else {
            "workdir"
        };
        out.push_str(&format!("- [{}/{}] {}\n", scope, m.kind.as_str(), m.text));
    }
    Some(out)
}

/// Cap on the AGENTS.md-style guidance file we inject as a system message.
/// 8 KB is enough for a dense conventions doc (tooling preferences, code style,
/// test commands, no-go zones) without rotting the model's context budget.
const AGENTS_MD_MAX_BYTES: usize = 8 * 1024;

/// Filenames we'll auto-load from the workdir root, in priority order. First
/// match wins — we never stack two (would double-bill the token budget).
const AGENTS_MD_FILENAMES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    ".claude/CLAUDE.md",
    ".cursor/rules",
];

pub fn try_load_agents_md(workdir: &str) -> Option<String> {
    let root = std::path::Path::new(workdir);
    for name in AGENTS_MD_FILENAMES {
        let p = root.join(name);
        if let Ok(mut content) = std::fs::read_to_string(&p) {
            let truncated = content.len() > AGENTS_MD_MAX_BYTES;
            if truncated {
                content.truncate(AGENTS_MD_MAX_BYTES);
                while !content.is_char_boundary(content.len()) {
                    content.pop();
                }
                content.push_str("\n…[truncated to 8 KB]");
            }
            return Some(format!(
                "# Project guidance loaded from `{name}` (workdir-level conventions — follow these unless the goal says otherwise)\n\n{content}",
            ));
        }
    }
    None
}

fn render_continuation(ev: &EventRecord) -> String {
    let attempt = ev
        .payload
        .get("attempt")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    let budget = ev
        .payload
        .get("budget")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    format!("System: continuation audit (attempt {attempt}/{budget}).\n\n{CONTINUATION_PROMPT}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_md_loaded_from_workdir_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("AGENTS.md"),
            "Use `cargo nextest run`, not `cargo test`.\nFile names are kebab-case.\n",
        )
        .unwrap();
        let got = try_load_agents_md(&dir.path().display().to_string()).unwrap();
        assert!(got.starts_with("# Project guidance loaded from `AGENTS.md`"));
        assert!(got.contains("cargo nextest"));
        assert!(!got.contains("[truncated"));
    }

    #[test]
    fn agents_md_falls_back_to_claude_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "Prefer apply_patch.\n").unwrap();
        let got = try_load_agents_md(&dir.path().display().to_string()).unwrap();
        assert!(got.contains("CLAUDE.md"));
        assert!(got.contains("Prefer apply_patch"));
    }

    #[test]
    fn agents_md_truncated_at_cap() {
        let dir = tempfile::tempdir().unwrap();
        // Big file: 12 KB of `x`s — well over the 8 KB cap.
        let big = "x".repeat(12 * 1024);
        std::fs::write(dir.path().join("AGENTS.md"), &big).unwrap();
        let got = try_load_agents_md(&dir.path().display().to_string()).unwrap();
        assert!(got.contains("[truncated to 8 KB]"));
        // The body should be capped — the wrapping header adds ~120 chars so
        // the total stays well under 9 KB.
        assert!(got.len() < 9 * 1024);
    }

    #[test]
    fn agents_md_absent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(try_load_agents_md(&dir.path().display().to_string()).is_none());
    }

    // § C.M-B — dialect prompt selection tests.

    #[test]
    fn system_prompt_for_json_returns_json_prompt() {
        assert_eq!(system_prompt_for(ToolDialect::Json), SYSTEM_PROMPT_JSON);
    }

    #[test]
    fn system_prompt_for_gemma4_strict_returns_strict_prompt() {
        assert_eq!(
            system_prompt_for(ToolDialect::Gemma4Strict),
            SYSTEM_PROMPT_GEMMA4_STRICT
        );
    }

    #[test]
    fn json_and_strict_prompts_differ() {
        assert_ne!(SYSTEM_PROMPT_JSON, SYSTEM_PROMPT_GEMMA4_STRICT);
    }

    #[test]
    fn gemma4_strict_prompt_includes_anti_drift_clauses() {
        let p = SYSTEM_PROMPT_GEMMA4_STRICT;
        // The three load-bearing constraints that distinguish this dialect
        // from the universal one.
        assert!(p.contains("No markdown"));
        assert!(p.contains("ONE JSON object"));
        assert!(p.contains("Anti-drift recovery"));
        assert!(p.contains("NEVER reply with a freeform markdown summary"));
    }

    #[test]
    fn gemma4_native_falls_back_to_strict_prompt() {
        // The Gemma4Native preprocessor (§ C.M-C) rewrites the model's
        // native envelopes into JSON before the parser sees them — so the
        // model itself still benefits from the strict-JSON framing.
        assert_eq!(
            system_prompt_for(ToolDialect::Gemma4Native),
            SYSTEM_PROMPT_GEMMA4_STRICT
        );
    }
}
