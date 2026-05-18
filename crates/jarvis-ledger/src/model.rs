use jarvis_core::{AgentId, EventId, TaskId};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Agent committed to attempting an action.
    Attempt,
    /// Observation returned by a tool or the environment.
    Observation,
    /// Agent's recorded decision/plan/reasoning.
    Decision,
    /// Agent invoked a tool with given args.
    ToolCall,
    /// Tool returned a result (success).
    ToolResult,
    /// An error event (any layer).
    Error,
    /// Final verdict for a task or subtask.
    Verdict,
    /// Agent spawned a subtask.
    Spawn,
    /// Streaming chunk from the LLM (low-resolution; collapsed on completion).
    LlmChunk,
    /// Periodic agent heartbeat / status snapshot.
    Heartbeat,
    /// Forced continuation — daemon injected an audit prompt after a premature
    /// `done` verdict. Rendered as a user message on the next turn. The agent
    /// either re-confirms `done` with evidence or resumes work.
    Continuation,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Attempt => "attempt",
            Self::Observation => "observation",
            Self::Decision => "decision",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Error => "error",
            Self::Verdict => "verdict",
            Self::Spawn => "spawn",
            Self::LlmChunk => "llm_chunk",
            Self::Heartbeat => "heartbeat",
            Self::Continuation => "continuation",
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for EventKind {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "attempt" => Self::Attempt,
            "observation" => Self::Observation,
            "decision" => Self::Decision,
            "tool_call" => Self::ToolCall,
            "tool_result" => Self::ToolResult,
            "error" => Self::Error,
            "verdict" => Self::Verdict,
            "spawn" => Self::Spawn,
            "llm_chunk" => Self::LlmChunk,
            "heartbeat" => Self::Heartbeat,
            "continuation" => Self::Continuation,
            _ => return Err(()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return Err(()),
        })
    }
}

/// Event ready to be appended. The ledger assigns `id` and `ts_micros`.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub task_id: TaskId,
    pub agent_id: Option<AgentId>,
    pub kind: EventKind,
    pub subject: Option<String>,
    pub payload: serde_json::Value,
    pub parent_evt: Option<EventId>,
}

impl NewEvent {
    pub fn new(task_id: TaskId, kind: EventKind, payload: serde_json::Value) -> Self {
        Self {
            task_id,
            agent_id: None,
            kind,
            subject: None,
            payload,
            parent_evt: None,
        }
    }
    pub fn with_agent(mut self, id: AgentId) -> Self {
        self.agent_id = Some(id);
        self
    }
    pub fn with_subject(mut self, s: impl Into<String>) -> Self {
        self.subject = Some(s.into());
        self
    }
    pub fn with_parent(mut self, p: EventId) -> Self {
        self.parent_evt = Some(p);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: EventId,
    pub ts_micros: i64,
    pub task_id: TaskId,
    pub agent_id: Option<AgentId>,
    pub kind: EventKind,
    pub subject: Option<String>,
    pub payload: serde_json::Value,
    pub parent_evt: Option<EventId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: TaskId,
    pub parent: Option<TaskId>,
    pub goal: String,
    pub status: TaskStatus,
    pub workdir: String,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub error: Option<String>,
    pub sandbox: String,         // "" if unset (M2 tasks); "native"|"docker"
    pub net_policy: String,      // "" if unset
    pub worktree_path: String,   // "" if no worktree was created
    pub worktree_branch: String, // "" if no worktree branch
}

/// Per-task runtime metadata set when SubmitTask is dispatched.
#[derive(Debug, Clone, Default)]
pub struct TaskRuntimeInfo {
    pub sandbox: String,
    pub net_policy: String,
    pub worktree_path: String,
    pub worktree_branch: String,
}
