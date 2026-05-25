//! Building the LLM input from goal + history + tool catalog.

use jarvis_core::{ChatMessage, ChatRole, ToolDialect};
use jarvis_ledger::{EventKind, EventRecord};
use jarvis_tools::ToolSchema;
use std::sync::LazyLock;

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
        ToolDialect::HermesXml => &SYSTEM_PROMPT_HERMES_XML,
        ToolDialect::LlamaPython => &SYSTEM_PROMPT_LLAMA_PYTHON,
        ToolDialect::ToolCodeBlock => &SYSTEM_PROMPT_TOOL_CODE_BLOCK,
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
observation with `tool: "hook:<phase>:<label>"` (phase ∈ `pre`/`post`/`on_error`). If a hook reports `exit != 0`
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
- **OPTIONAL but ENFORCED:** add a top-level `"phase"` field to ANY reply
  to declare the lifecycle phase the next step belongs to. Accepted
  values: `"plan"` (read-only exploration), `"act"` (writing changes),
  `"verify"` (running tests / hooks), `"ship"` (final commit / publish).
  When you switch phase, set it on that turn's reply; the previous phase
  carries over when `phase` is omitted. The SPA renders phase transitions
  on the timeline. **While the current phase is `plan`, any tool that has
  side-effects (apply_patch, fs_write, shell, …) is refused** with an
  observation; switch phase to `act` on the next decision to unlock
  writes. Read-only tools (fs_read, grep, glob, repo_map, web_search, …)
  are always allowed.
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
observation with `tool: "hook:<phase>:<label>"` (phase ∈ `pre`/`post`/`on_error`). If a hook reports `exit != 0` or
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

/// § T1.8 — shared body for the non-JSON dialect prompts.
///
/// Holds the platform / verification / planning / rules sections used by the
/// non-JSON dialect prompts. Composed into each dialect prompt via `format!()`
/// so each variant keeps its envelope-specific header + footer.
pub const COMMON_DIALECT_SECTIONS: &str = r#"# Decide before you act
First, ask yourself: does this goal actually require touching the file
system or running shell commands?

- **Informational / general-knowledge questions** ("What is …", "Explain
  …", "How would you …", a request for weather, news, travel advice,
  trivia, code that doesn't reference any file in the workdir): answer
  directly from your training knowledge. Emit the "done" envelope on the
  FIRST turn. Do NOT call any tool. The workdir is irrelevant to these
  questions — scanning it is wasted work.

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
corrupt UTF-8 files via the system ANSI codepage (CP1252). `fs_read`
reads bytes directly through Rust's UTF-8 decoder and is safe.

**Searching: use `grep` and `glob`, not shell.**
Both automatically skip `.gitignore`'d paths, hidden dirs, and binary files.

**Verification hooks (lifecycle).**
The daemon may auto-run project-configured hooks around tool invocations
(e.g. `cargo check` post `apply_patch`). Their output arrives as an
observation with `tool: "hook:<phase>:<label>"` (phase ∈ `pre`/`post`/
`on_error`). If a hook reports `exit != 0` or any error in stderr, treat
it as a HARD signal that your last edit broke something — fix it before
continuing. Do not declare done while a hook is failing.

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
- For **edits**, prefer `apply_patch` over `fs_write` for cheaper diffs.
- When the goal is achieved, finalize with a "done" envelope carrying the summary.
- When the goal is impossible or unsafe, finalize with a "fail" envelope carrying the reason.
- Available tools and their JSON-schema args are listed below.
"#;

/// § T1.8 — Hermes / Qwen 2.5/3 / Mistral fine-tune XML dialect.
///
/// The model wraps a JSON tool call in `<tool_call>...</tool_call>`. The
/// preprocessor (`rewrite_hermes_xml`) translates it into the universal
/// JSON envelope before the parser sees it.
pub static SYSTEM_PROMPT_HERMES_XML: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{header}{common}{footer}",
        header = HERMES_XML_HEADER,
        common = COMMON_DIALECT_SECTIONS,
        footer = "\nREMEMBER: ONE <tool_call> envelope per reply. Nothing outside the envelope.\n",
    )
});

const HERMES_XML_HEADER: &str = r#"You are Jarvis, an autonomous coding agent. You operate by emitting ONE structured action per turn.

# Output format — Hermes / Qwen XML

To call a tool, reply with EXACTLY ONE `<tool_call>` envelope wrapping a JSON object:

<tool_call>
{"name": "<tool name>", "arguments": { ... }}
</tool_call>

No prose outside the envelope. The JSON object inside MUST have a `name`
field and an `arguments` object (use `{}` when the tool takes no args).

To finalize, reply with EXACTLY ONE `<tool_call>` envelope where `name` is
`done` (or `fail`) and `arguments` carries the user-facing summary:

<tool_call>
{"name": "done", "arguments": {"message": "<final answer in markdown>"}}
</tool_call>

The `message` field accepts markdown (bullets, headers, `code`); JSON
strings need real newlines escaped as `\n`.

Examples (copy the SHAPE, not the contents):

<tool_call>
{"name": "fs_read", "arguments": {"path": "src/main.rs"}}
</tool_call>

<tool_call>
{"name": "done", "arguments": {"message": "Added a paragraph to README §API. File: README.md lines 42-48."}}
</tool_call>

"#;

/// § T1.8 — Llama 3.1+ python-tag dialect.
///
/// The model emits `<|python_tag|>fn(args)<|eom_id|>`. The preprocessor
/// (`rewrite_llama_python`) translates it into the universal JSON envelope.
pub static SYSTEM_PROMPT_LLAMA_PYTHON: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{header}{common}{footer}",
        header = LLAMA_PYTHON_HEADER,
        common = COMMON_DIALECT_SECTIONS,
        footer =
            "\nREMEMBER: ONE <|python_tag|> envelope per reply. Nothing outside the envelope.\n",
    )
});

const LLAMA_PYTHON_HEADER: &str = r#"You are Jarvis, an autonomous coding agent. You operate by emitting ONE structured action per turn.

# Output format — Llama python-tag

To call a tool, reply with EXACTLY ONE `<|python_tag|>` envelope wrapping a
Python-style call expression and terminated by `<|eom_id|>`:

<|python_tag|>fn_name(arg1="value", arg2=42, flag=true)<|eom_id|>

No prose outside the envelope. Argument values use Python literals:
strings in double or single quotes, integers, floats, `true` / `false`,
`None`. The function name is the tool name.

To finalize, call the special tool `done` (or `fail`) with the user-facing
summary as the `message` argument:

<|python_tag|>done(message="Added a paragraph to README §API. File: README.md lines 42-48.")<|eom_id|>

The `message` value accepts markdown (bullets, headers, `code`); use `\n`
inside the quoted string for newlines.

Examples (copy the SHAPE, not the contents):

<|python_tag|>fs_read(path="src/main.rs")<|eom_id|>
<|python_tag|>done(message="All tests pass: cargo test reports 47 passed, 0 failed.")<|eom_id|>

"#;

/// § T1.8 — Generic tool_code fenced-block dialect.
///
/// The model emits a fenced code block tagged `tool_code` (preferred) or
/// `python` (accepted as a fallback when the body is a call expression).
/// The preprocessor (`rewrite_tool_code_block`) translates it into the
/// universal JSON envelope.
pub static SYSTEM_PROMPT_TOOL_CODE_BLOCK: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{header}{common}{footer}",
        header = TOOL_CODE_BLOCK_HEADER,
        common = COMMON_DIALECT_SECTIONS,
        footer =
            "\nREMEMBER: ONE ```tool_code fenced block per reply. Nothing outside the fence.\n",
    )
});

const TOOL_CODE_BLOCK_HEADER: &str = r#"You are Jarvis, an autonomous coding agent. You operate by emitting ONE structured action per turn.

# Output format — tool_code fenced block

To call a tool, reply with EXACTLY ONE fenced code block tagged `tool_code`
wrapping a Python-style call expression:

```tool_code
fn_name(arg1="value", arg2=42, flag=true)
```

No prose outside the fence. Argument values use Python literals: strings in
double or single quotes, integers, floats, `true` / `false`, `None`. The
function name is the tool name.

To finalize, call the special tool `done` (or `fail`) with the user-facing
summary as the `message` argument:

```tool_code
done(message="Added a paragraph to README §API. File: README.md lines 42-48.")
```

The `message` value accepts markdown (bullets, headers, `code`); use `\n`
inside the quoted string for newlines.

Examples (copy the SHAPE, not the contents):

```tool_code
fs_read(path="src/main.rs")
```

```tool_code
done(message="All tests pass: cargo test reports 47 passed, 0 failed.")
```

"#;

fn estimate_tokens(s: &str) -> usize {
    s.len() / 3
}

#[allow(clippy::too_many_arguments)] // each arg is an independent prompt input
pub fn build_messages(
    goal: &str,
    workdir: &str,
    tools: &[ToolSchema],
    history: &[EventRecord],
    memories: &[jarvis_ledger::MemoryRecord],
    dialect: ToolDialect,
    ancestor_goals: &[String],
    thinking: bool,
    ctx_len: u32,
    lazy_tool_catalog: bool,
) -> Vec<ChatMessage> {
    // Dynamic sliding window context budget: limit overall context to 80% of Picked Model's capacity
    let budget = (ctx_len as usize * 80) / 100;

    let system = if thinking {
        format!("<|think|>\n{}", system_prompt_for(dialect))
    } else {
        system_prompt_for(dialect).to_string()
    };

    let catalog = if lazy_tool_catalog {
        render_tool_catalog_compact(tools)
    } else {
        render_tool_catalog(tools)
    };

    let agents_md = try_load_agents_md(workdir).unwrap_or_default();

    let mem_msg = render_memories(memories).unwrap_or_default();

    let skills_msg = render_active_skills(workdir);

    let plan_msg = render_latest_plan(history).unwrap_or_default();

    let user_turn = if ancestor_goals.is_empty() {
        format!("Goal: {goal}\nWorkdir: {workdir}\n\nBegin.")
    } else {
        let mut prior = String::new();
        for (i, g) in ancestor_goals.iter().enumerate() {
            prior.push_str(&format!("  {}. {}\n", i + 1, g));
        }
        format!(
            "Goal: {goal}\nWorkdir: {workdir}\n\nThis is a follow-up turn in a multi-step conversation. Prior turn goals (oldest → newest):\n{prior}\nThe ASSISTANT and USER messages that follow are the recorded conversation history (decisions and tool observations from those prior turns).\n\nThe goal stated above is the user's NEW request. Use the prior turns ONLY to resolve what the new request refers to — for example \"et a Marseille ?\" after a weather question means: get the weather FOR MARSEILLE.\n\nCRITICAL — the new goal asks about something DIFFERENT from the prior turns. Do NOT copy or repeat a previous turn's answer. If the new goal needs fresh data (a different city, a different file, a different computation), you MUST call the appropriate tools again for the NEW goal. Only answer directly without tools if the prior turns ALREADY contain the exact answer to this specific new goal.\n\nBegin."
        )
    };

    // Calculate static parts size to compute remaining budget for history
    let static_size = estimate_tokens(&system)
        + estimate_tokens(&catalog)
        + estimate_tokens(&agents_md)
        + estimate_tokens(&mem_msg)
        + estimate_tokens(&skills_msg)
        + estimate_tokens(&plan_msg)
        + estimate_tokens(&user_turn);

    let history_budget = budget.saturating_sub(static_size);

    // Keep history events chronologically but build them newest-first to slide window
    let mut selected_history = Vec::new();
    let mut current_history_tokens = 0;

    for ev in history.iter().rev() {
        if is_update_plan_event(ev) {
            continue;
        }

        let msg_str = match ev.kind {
            EventKind::Decision => render_decision(ev),
            EventKind::ToolResult | EventKind::Error => render_observation(ev),
            EventKind::Continuation => render_continuation(ev),
            _ => continue,
        };

        let est = estimate_tokens(&msg_str);
        if current_history_tokens + est > history_budget {
            // Keep at least the single most recent history event so the agent is never completely blind to the last step
            if selected_history.is_empty() {
                selected_history.push((ev, msg_str));
            }
            break;
        }

        current_history_tokens += est;
        selected_history.push((ev, msg_str));
    }

    selected_history.reverse();

    // Assemble final ChatMessages
    let mut msgs = Vec::with_capacity(selected_history.len() + 7);
    msgs.push(ChatMessage {
        role: ChatRole::System,
        content: system,
    });
    msgs.push(ChatMessage {
        role: ChatRole::System,
        content: catalog,
    });

    if !agents_md.is_empty() {
        msgs.push(ChatMessage {
            role: ChatRole::System,
            content: agents_md,
        });
    }

    if !skills_msg.is_empty() {
        msgs.push(ChatMessage {
            role: ChatRole::System,
            content: skills_msg,
        });
    }

    if !mem_msg.is_empty() {
        msgs.push(ChatMessage {
            role: ChatRole::System,
            content: mem_msg,
        });
    }

    if !plan_msg.is_empty() {
        msgs.push(ChatMessage {
            role: ChatRole::System,
            content: plan_msg,
        });
    }

    msgs.push(ChatMessage {
        role: ChatRole::User,
        content: user_turn,
    });

    for (ev, content) in selected_history {
        match ev.kind {
            EventKind::Decision => msgs.push(ChatMessage {
                role: ChatRole::Assistant,
                content,
            }),
            EventKind::ToolResult | EventKind::Error => msgs.push(ChatMessage {
                role: ChatRole::User,
                content,
            }),
            EventKind::Continuation => msgs.push(ChatMessage {
                role: ChatRole::User,
                content,
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

/// § T1.3 — compact catalog: only names + descriptions, no full JSON schemas.
/// The agent must call `search_tools(query)` to retrieve the full args schema
/// of any tool it wants to invoke. Trades ~3–5 k tokens / turn against one
/// extra round-trip on first use of an unfamiliar tool. Always renders
/// `search_tools` itself in full so the model knows how to query.
fn render_tool_catalog_compact(tools: &[ToolSchema]) -> String {
    let mut s = String::from(
        "# Available tools (compact catalog)\n\n\
         Only tool names + descriptions are shown below. To learn the full \
         JSON-schema `args` of any tool, call `search_tools(query=\"<name or \
         keyword>\")` — it returns the matching schemas you need to invoke \
         the tool correctly.\n\n",
    );
    for t in tools {
        if t.name == "search_tools" {
            // Always render `search_tools` in full so the model can call it
            // without a chicken-and-egg search.
            s.push_str(&format!("## {}\n{}\n", t.name, t.description));
            s.push_str("args schema:\n```json\n");
            s.push_str(&serde_json::to_string_pretty(&t.args_schema).unwrap_or_default());
            s.push_str("\n```\n\n");
        } else {
            s.push_str(&format!("- **{}** — {}\n", t.name, t.description));
        }
    }
    s
}

/// Render a past decision for the conversation history.
///
/// § C.M-E — the Gemma 4 model card is explicit: "in multi-turn
/// conversations, the model's historical output should include only the
/// final response. Previous turn reflections must not be added before the
/// next user turn." The `thought` field IS that reflection — so we strip
/// it here. The decision still carries its load-bearing parts (the
/// action plus tool/args, or the done/fail message) so the model sees
/// what it did, just not the reasoning that led there. The full payload
/// (thought included) stays in the ledger and the SPA timeline — only
/// the LLM-facing history is trimmed.
fn render_decision(ev: &EventRecord) -> String {
    let p = &ev.payload;
    let action = p.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let mut out = serde_json::Map::new();
    if !action.is_empty() {
        out.insert("action".to_string(), serde_json::json!(action));
    }
    // Tool calls: keep tool + args so the observation that follows makes
    // sense. Done/fail: keep the message (the final answer).
    if let Some(tool) = p.get("tool").filter(|v| !v.is_null()) {
        out.insert("tool".to_string(), tool.clone());
    }
    if let Some(args) = p.get("args").filter(|v| !v.is_null()) {
        out.insert("args".to_string(), args.clone());
    }
    if let Some(msg) = p.get("message").filter(|v| !v.is_null()) {
        out.insert("message".to_string(), msg.clone());
    }
    serde_json::to_string(&serde_json::Value::Object(out)).unwrap_or_else(|_| "{}".to_string())
}

fn compact_json_value(val: &serde_json::Value) -> serde_json::Value {
    match val {
        serde_json::Value::String(s) => {
            if s.len() > 4000 {
                let mut prefix_end = 1000;
                while prefix_end > 0 && !s.is_char_boundary(prefix_end) {
                    prefix_end -= 1;
                }
                let mut suffix_start = s.len() - 1000;
                while suffix_start < s.len() && !s.is_char_boundary(suffix_start) {
                    suffix_start += 1;
                }
                let first_part = &s[..prefix_end];
                let last_part = &s[suffix_start..];
                let truncated_len = s.len() - prefix_end - (s.len() - suffix_start);
                serde_json::Value::String(format!(
                    "{}\n\n... [TRUNCATED {} BYTES FOR CONTEXT EFFICIENCY] ...\n\n{}",
                    first_part, truncated_len, last_part
                ))
            } else {
                val.clone()
            }
        }
        serde_json::Value::Array(arr) => {
            let compacted: Vec<serde_json::Value> = arr.iter().map(compact_json_value).collect();
            serde_json::Value::Array(compacted)
        }
        serde_json::Value::Object(obj) => {
            let mut compacted = serde_json::Map::new();
            for (k, v) in obj {
                compacted.insert(k.clone(), compact_json_value(v));
            }
            serde_json::Value::Object(compacted)
        }
        _ => val.clone(),
    }
}

fn render_observation(ev: &EventRecord) -> String {
    let mut s = String::new();
    s.push_str("Observation:\n");
    s.push_str("```json\n");
    let compacted = compact_json_value(&ev.payload);
    s.push_str(&serde_json::to_string_pretty(&compacted).unwrap_or_default());
    s.push_str("\n```");
    s
}

/// § T2.1 — render active memories grouped + prioritised by layer.
/// Procedural and semantic layers are always included (up to their
/// caps); episodic layer is recency-only and tightly capped; archival
/// is skipped entirely (the prompt has higher-signal sources). Layer
/// labels appear in the rendered bullets so the model can weight
/// procedural patterns over single-shot facts when they conflict.
///
/// Returns `None` when no memory survives the filter.
fn render_memories(memories: &[jarvis_ledger::MemoryRecord]) -> Option<String> {
    use jarvis_ledger::MemoryLayer;
    if memories.is_empty() {
        return None;
    }
    // Per-layer caps so a flood of one type doesn't crowd out the
    // others. Numbers were picked from the existing 32-row cap +
    // post-Hermes guidance (procedural is highest-value for an agent
    // that runs the same workflows repeatedly).
    let cap_procedural = 16;
    let cap_semantic = 12;
    let cap_episodic = 4;

    let mut procedural = Vec::new();
    let mut semantic = Vec::new();
    let mut episodic = Vec::new();
    // We iterate the input newest-first (the caller already orders
    // updated_at DESC) so the per-layer take() picks the freshest.
    for m in memories {
        match m.layer {
            MemoryLayer::Procedural if procedural.len() < cap_procedural => {
                procedural.push(m);
            }
            MemoryLayer::Semantic if semantic.len() < cap_semantic => {
                semantic.push(m);
            }
            MemoryLayer::Episodic if episodic.len() < cap_episodic => {
                episodic.push(m);
            }
            // Working / archival never render. Working is turn-local
            // (caller should evict it before prompt assembly); archival
            // is intentionally cold storage.
            _ => {}
        }
    }
    if procedural.is_empty() && semantic.is_empty() && episodic.is_empty() {
        return None;
    }

    let mut out = String::from(
        "Learned constraints for this workdir (curated by the user — treat as authoritative; layered: procedural > semantic > episodic):\n",
    );
    let push_section = |out: &mut String, label: &str, items: &[&jarvis_ledger::MemoryRecord]| {
        if items.is_empty() {
            return;
        }
        for m in items {
            let scope = if matches!(m.scope, jarvis_ledger::MemoryScope::Global) {
                "global"
            } else {
                "workdir"
            };
            out.push_str(&format!(
                "- [{}/{}/{}] {}\n",
                label,
                scope,
                m.kind.as_str(),
                m.text
            ));
        }
    };
    push_section(&mut out, "procedural", &procedural);
    push_section(&mut out, "semantic", &semantic);
    push_section(&mut out, "episodic", &episodic);
    Some(out)
}

/// Cap on the AGENTS.md-style guidance we inject as a system message.
/// 8 KB total budget across the whole cascade (repo root → workdir).
const AGENTS_MD_MAX_BYTES: usize = 8 * 1024;

/// Fallback filenames checked at the workdir level when no `AGENTS.md` exists
/// anywhere in the cascade. Order matters: first match wins.
const AGENTS_MD_FALLBACK_FILENAMES: &[&str] = &["CLAUDE.md", ".claude/CLAUDE.md", ".cursor/rules"];

/// Walk parents from `start` upward looking for a `.git` entry. Returns the
/// first directory that contains one, or `None` if the walk exits the
/// filesystem without finding a repo.
fn find_repo_root(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Build the cascade path: every directory from `repo_root` down to `workdir`
/// inclusive. When `workdir` is not under `repo_root` (or there is no repo),
/// the cascade reduces to `[workdir]`.
fn cascade_dirs(repo_root: &std::path::Path, workdir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![repo_root.to_path_buf()];
    if let Ok(rel) = workdir.strip_prefix(repo_root) {
        let mut p = repo_root.to_path_buf();
        for comp in rel.components() {
            p.push(comp);
            // Deduplicate: when workdir == repo_root, `rel` is empty and we
            // never enter the loop. When it differs, every push yields a
            // distinct directory.
            dirs.push(p.clone());
        }
    } else if workdir != repo_root {
        dirs.push(workdir.to_path_buf());
    }
    dirs
}

/// § T2.2 — render the active self-authored skills as a single system
/// message. Each skill contributes its title + trigger + body, framed
/// so the agent treats them as "things YOU figured out before; reuse
/// them when the trigger fires". Active skills live under
/// `<workdir>/.jarvis/skills/active/*.md` — see `skill_extractor` for
/// the format. Returns empty when no skills are active.
const SKILLS_MAX_BYTES: usize = 16 * 1024;

fn render_active_skills(workdir: &str) -> String {
    let skills = crate::skill_extractor::load_active_skills(workdir);
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "# Active skills (self-authored from past tasks in this workdir — reuse them when the trigger matches)\n",
    );
    let mut remaining = SKILLS_MAX_BYTES.saturating_sub(out.len());
    let total = skills.len();
    let mut emitted = 0;
    for s in skills.into_iter() {
        let header = format!(
            "\n## `{name}` — {title}\n**Trigger:** {trigger}\n\n",
            name = s.name,
            title = s.title,
            trigger = s.trigger,
        );
        if remaining <= header.len() + 64 {
            break;
        }
        out.push_str(&header);
        remaining = remaining.saturating_sub(header.len());
        if s.body.len() <= remaining {
            out.push_str(&s.body);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            remaining = remaining.saturating_sub(s.body.len() + 1);
        } else {
            let mut clip = remaining;
            while clip > 0 && !s.body.is_char_boundary(clip) {
                clip -= 1;
            }
            out.push_str(&s.body[..clip]);
            out.push_str("\n…[skill body truncated]\n");
            remaining = 0;
        }
        emitted += 1;
    }
    if emitted < total {
        out.push_str(&format!(
            "\n…[skipped {} more skill(s) — over budget]\n",
            total - emitted
        ));
    }
    out
}

/// § T1.1 — load AGENTS.md hierarchically (repo root → subdirs → workdir) and
/// concatenate the chain into a single system message, capped at
/// [`AGENTS_MD_MAX_BYTES`]. Later (more specific) sections appear last so
/// they take precedence in the model's read order.
///
/// Behaviour:
/// - Walks up from `workdir` to locate the git repo root (`.git/`). When the
///   workdir is not in a repo, the workdir itself is the only level.
/// - At each level, looks for `AGENTS.md`. Each hit becomes a section
///   labelled by its repo-relative path.
/// - If the cascade is empty, falls back to `CLAUDE.md`,
///   `.claude/CLAUDE.md`, `.cursor/rules` at the workdir level only — the
///   pre-existing single-file behaviour for projects that haven't adopted
///   AGENTS.md yet.
/// - Total output is capped at 8 KB. Sections are truncated in order; once
///   the budget runs out, subsequent (more specific) sections are skipped
///   with an explicit notice.
pub fn try_load_agents_md(workdir: &str) -> Option<String> {
    let workdir_path = std::path::Path::new(workdir);
    let repo_root = find_repo_root(workdir_path).unwrap_or_else(|| workdir_path.to_path_buf());

    let dirs = cascade_dirs(&repo_root, workdir_path);
    let mut sections: Vec<(String, String)> = Vec::new();
    for dir in &dirs {
        let candidate = dir.join("AGENTS.md");
        if let Ok(content) = std::fs::read_to_string(&candidate) {
            let label = candidate
                .strip_prefix(&repo_root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| candidate.display().to_string());
            // Normalize separators for stable rendering across platforms.
            let label = label.replace('\\', "/");
            sections.push((label, content));
        }
    }

    if sections.is_empty() {
        for name in AGENTS_MD_FALLBACK_FILENAMES {
            let p = workdir_path.join(name);
            if let Ok(content) = std::fs::read_to_string(&p) {
                sections.push(((*name).to_string(), content));
                break;
            }
        }
    }

    if sections.is_empty() {
        return None;
    }

    let header = if sections.len() == 1 {
        format!(
            "# Project guidance loaded from `{}` (follow these unless the goal says otherwise)\n\n",
            sections[0].0
        )
    } else {
        String::from(
            "# Project guidance (AGENTS.md cascade — repo root to most specific; later sections override earlier ones)\n\n",
        )
    };
    let mut out = header;
    let mut remaining = AGENTS_MD_MAX_BYTES.saturating_sub(out.len());
    let total_sections = sections.len();
    let mut emitted = 0usize;
    for (label, content) in sections.into_iter() {
        let section_header = if total_sections == 1 {
            String::new()
        } else {
            format!("\n## `{label}`\n\n")
        };
        // Need room for the header plus a meaningful body slice; otherwise
        // bail out — the remaining sections won't fit either.
        if remaining <= section_header.len() + 64 {
            break;
        }
        out.push_str(&section_header);
        remaining = remaining.saturating_sub(section_header.len());

        if content.len() <= remaining {
            out.push_str(&content);
            remaining = remaining.saturating_sub(content.len());
            if !out.ends_with('\n') {
                out.push('\n');
                remaining = remaining.saturating_sub(1);
            }
        } else {
            let mut clipped_end = remaining;
            while clipped_end > 0 && !content.is_char_boundary(clipped_end) {
                clipped_end -= 1;
            }
            out.push_str(&content[..clipped_end]);
            out.push_str("\n…[truncated to 8 KB]");
            remaining = 0;
        }
        emitted += 1;
    }
    if emitted < total_sections {
        out.push_str(&format!(
            "\n…[skipped {} more section(s) — over 8 KB budget]\n",
            total_sections - emitted
        ));
    }

    Some(out)
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

    // § T1.1 — AGENTS.md hierarchical cascade tests.

    #[test]
    fn agents_md_cascade_combines_repo_and_subdir() {
        // Repo layout:
        //   <root>/.git/
        //   <root>/AGENTS.md         ← repo-wide conventions
        //   <root>/crates/foo/
        //   <root>/crates/foo/AGENTS.md   ← crate-specific conventions
        // Workdir = <root>/crates/foo. Both sections must appear, in cascade
        // order (root first, most-specific last).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join("AGENTS.md"),
            "Always run `cargo fmt` before commit.\n",
        )
        .unwrap();
        let subdir = root.join("crates").join("foo");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("AGENTS.md"), "This crate forbids `unwrap()`.\n").unwrap();

        let got = try_load_agents_md(&subdir.display().to_string()).unwrap();
        // Cascade header is used when more than one section is present.
        assert!(got.contains("AGENTS.md cascade"), "got: {got}");
        // Both labels appear, with the deeper one labelled relative to repo.
        assert!(got.contains("`AGENTS.md`"), "got: {got}");
        assert!(got.contains("crates/foo/AGENTS.md"), "got: {got}");
        // Both bodies are present.
        assert!(got.contains("cargo fmt"));
        assert!(got.contains("forbids `unwrap()`"));
        // Root section appears before the subdir section (so the more
        // specific guidance is read last and takes precedence).
        let root_pos = got.find("Always run").unwrap();
        let sub_pos = got.find("forbids").unwrap();
        assert!(root_pos < sub_pos, "root must precede subdir in cascade");
    }

    #[test]
    fn agents_md_cascade_single_repo_level_keeps_single_header() {
        // Only one AGENTS.md exists in the cascade → behaves like the
        // workdir-level case: the single-section header is used, not the
        // cascade header.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let subdir = root.join("crates").join("foo");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(root.join("AGENTS.md"), "Top-level rule.\n").unwrap();

        let got = try_load_agents_md(&subdir.display().to_string()).unwrap();
        assert!(got.starts_with("# Project guidance loaded from `AGENTS.md`"));
        assert!(!got.contains("cascade"));
        assert!(got.contains("Top-level rule"));
    }

    #[test]
    fn agents_md_cascade_skips_sections_over_budget() {
        // Two AGENTS.md, the first one alone consumes the entire 8 KB
        // budget → the second one is reported as skipped, not silently
        // dropped.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "x".repeat(20 * 1024)).unwrap();
        let subdir = root.join("sub");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("AGENTS.md"), "This will be skipped.\n").unwrap();

        let got = try_load_agents_md(&subdir.display().to_string()).unwrap();
        assert!(got.contains("[truncated to 8 KB]"));
        assert!(got.contains("skipped 1 more section"));
        assert!(!got.contains("This will be skipped"));
    }

    #[test]
    fn agents_md_workdir_only_no_repo() {
        // Only an AGENTS.md at the workdir level. The label may be
        // `AGENTS.md` (no `.git` in any ancestor of the tempdir) or a
        // relative path under whatever ancestor `.git` happens to exist on
        // the host — either way, exactly one section is rendered and its
        // body shows up.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Workdir-only rule.\n").unwrap();
        let got = try_load_agents_md(&dir.path().display().to_string()).unwrap();
        assert!(got.contains("AGENTS.md"));
        assert!(got.contains("Workdir-only rule"));
        // Single section → no cascade banner and no skip notice.
        assert!(!got.contains("AGENTS.md cascade"));
        assert!(!got.contains("skipped"));
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

    // § T1.8 — open-weights dialect prompt selection.

    #[test]
    fn system_prompt_for_hermes_xml_returns_hermes_prompt() {
        assert_eq!(
            system_prompt_for(ToolDialect::HermesXml),
            SYSTEM_PROMPT_HERMES_XML.as_str()
        );
    }

    #[test]
    fn system_prompt_for_llama_python_returns_llama_prompt() {
        assert_eq!(
            system_prompt_for(ToolDialect::LlamaPython),
            SYSTEM_PROMPT_LLAMA_PYTHON.as_str()
        );
    }

    #[test]
    fn system_prompt_for_tool_code_block_returns_tool_code_prompt() {
        assert_eq!(
            system_prompt_for(ToolDialect::ToolCodeBlock),
            SYSTEM_PROMPT_TOOL_CODE_BLOCK.as_str()
        );
    }

    #[test]
    fn open_weights_prompts_carry_format_specific_markers() {
        // Each prompt MUST describe the envelope the corresponding
        // rewriter recognises — otherwise the model is briefed for one
        // dialect and parsed as another.
        assert!(SYSTEM_PROMPT_HERMES_XML.contains("<tool_call>"));
        assert!(SYSTEM_PROMPT_HERMES_XML.contains("</tool_call>"));
        assert!(SYSTEM_PROMPT_LLAMA_PYTHON.contains("<|python_tag|>"));
        assert!(SYSTEM_PROMPT_LLAMA_PYTHON.contains("<|eom_id|>"));
        assert!(SYSTEM_PROMPT_TOOL_CODE_BLOCK.contains("```tool_code"));
    }

    #[test]
    fn open_weights_prompts_share_common_sections() {
        // All three reuse the shared body so the operational guidance
        // (planning, verification hooks, platform awareness) stays in sync.
        for p in [
            SYSTEM_PROMPT_HERMES_XML.as_str(),
            SYSTEM_PROMPT_LLAMA_PYTHON.as_str(),
            SYSTEM_PROMPT_TOOL_CODE_BLOCK.as_str(),
        ] {
            assert!(p.contains("# Decide before you act"));
            assert!(p.contains("# Platform awareness"));
            assert!(p.contains("update_plan"));
        }
    }

    // § T2.1 — layered memory rendering.

    fn mk_mem(
        id: i64,
        text: &str,
        layer: jarvis_ledger::MemoryLayer,
        kind: jarvis_ledger::MemoryKind,
    ) -> jarvis_ledger::MemoryRecord {
        jarvis_ledger::MemoryRecord {
            id,
            scope: jarvis_ledger::MemoryScope::Workdir,
            scope_value: "/repo".to_string(),
            kind,
            layer,
            text: text.to_string(),
            status: jarvis_ledger::MemoryStatus::Active,
            source_task_id: None,
            created_at: 0,
            updated_at: 0,
            usage_count: 0,
        }
    }

    #[test]
    fn render_memories_returns_none_for_empty() {
        assert!(render_memories(&[]).is_none());
    }

    #[test]
    fn render_memories_groups_by_layer_with_label() {
        use jarvis_ledger::{MemoryKind, MemoryLayer};
        let mems = vec![
            mk_mem(
                1,
                "use cargo check",
                MemoryLayer::Procedural,
                MemoryKind::Pattern,
            ),
            mk_mem(
                2,
                "main branch is develop",
                MemoryLayer::Semantic,
                MemoryKind::Fact,
            ),
            mk_mem(
                3,
                "user prefers FR",
                MemoryLayer::Semantic,
                MemoryKind::Preference,
            ),
            mk_mem(
                4,
                "retried 3x on flaky test",
                MemoryLayer::Episodic,
                MemoryKind::Fact,
            ),
        ];
        let out = render_memories(&mems).unwrap();
        assert!(out.contains("[procedural/"));
        assert!(out.contains("use cargo check"));
        assert!(out.contains("[semantic/"));
        assert!(out.contains("main branch is develop"));
        assert!(out.contains("[episodic/"));
        // Procedural before semantic before episodic.
        let p_pos = out.find("[procedural/").unwrap();
        let s_pos = out.find("[semantic/").unwrap();
        let e_pos = out.find("[episodic/").unwrap();
        assert!(p_pos < s_pos);
        assert!(s_pos < e_pos);
    }

    #[test]
    fn render_memories_skips_working_and_archival_layers() {
        use jarvis_ledger::{MemoryKind, MemoryLayer};
        let mems = vec![
            mk_mem(1, "transient note", MemoryLayer::Working, MemoryKind::Fact),
            mk_mem(2, "old fact", MemoryLayer::Archival, MemoryKind::Fact),
        ];
        // No procedural/semantic/episodic → render returns None.
        assert!(render_memories(&mems).is_none());
    }

    #[test]
    fn render_memories_caps_episodic_tight() {
        use jarvis_ledger::{MemoryKind, MemoryLayer};
        // Push 10 episodic entries; only 4 should render.
        let mems: Vec<_> = (0..10)
            .map(|i| {
                mk_mem(
                    i,
                    &format!("ep #{i}"),
                    MemoryLayer::Episodic,
                    MemoryKind::Fact,
                )
            })
            .collect();
        let out = render_memories(&mems).unwrap();
        let count = out.matches("[episodic/").count();
        assert_eq!(count, 4);
    }
}
