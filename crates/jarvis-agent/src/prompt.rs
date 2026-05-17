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
