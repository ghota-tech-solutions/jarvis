//! Building the LLM input from goal + history + tool catalog.

use jarvis_core::{ChatMessage, ChatRole};
use jarvis_ledger::{EventKind, EventRecord};
use jarvis_tools::ToolSchema;

pub const SYSTEM_PROMPT: &str = r#"You are Jarvis, an autonomous coding agent. You operate by emitting ONE structured action per turn.

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

# Rules
- Take ONE action per turn. Wait for the observation before deciding the next step.
- Stay inside the workdir. Never read/write paths above it.
- Prefer small, verifiable steps. Read before you write.
- When the goal is achieved, emit { "action": "done", "message": "<what was done>" }.
- When the goal is impossible or unsafe, emit { "action": "fail", "message": "<why>" }.
- Available tools and their JSON-schema args are listed below.
"#;

pub fn build_messages(
    goal: &str,
    workdir: &str,
    tools: &[ToolSchema],
    history: &[EventRecord],
) -> Vec<ChatMessage> {
    let mut msgs = Vec::with_capacity(history.len() + 4);
    msgs.push(ChatMessage {
        role: ChatRole::System,
        content: SYSTEM_PROMPT.to_string(),
    });
    msgs.push(ChatMessage {
        role: ChatRole::System,
        content: render_tool_catalog(tools),
    });
    msgs.push(ChatMessage {
        role: ChatRole::User,
        content: format!("Goal: {goal}\nWorkdir: {workdir}\n\nBegin."),
    });

    for ev in history {
        match ev.kind {
            EventKind::Decision => msgs.push(ChatMessage {
                role: ChatRole::Assistant,
                content: render_decision(ev),
            }),
            EventKind::ToolResult | EventKind::Error => msgs.push(ChatMessage {
                role: ChatRole::User,
                content: render_observation(ev),
            }),
            _ => {}
        }
    }

    msgs
}

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
