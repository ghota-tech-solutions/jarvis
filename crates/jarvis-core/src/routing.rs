use crate::ProviderName;
use serde::{Deserialize, Serialize};

/// User-facing routing policy. Set per-task or in config defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum RoutingPolicy {
    /// Automatic selection by the router.
    #[default]
    Auto,
    /// Only providers under `[providers.local.*]` are eligible.
    LocalOnly,
    /// Only providers under `[providers.remote.*]` are eligible.
    RemoteOnly,
    /// Force a specific provider; fail if unavailable.
    Model(ProviderName),
}

/// Categories used by the router to apply Auto rules.
/// M1 uses only `Simple` and `Planning`; expanded in M5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Simple,
    Planning,
    DeepRefactor,
    Architecture,
    Reviewer,
    SimpleEdit,
    Summarize,
    Classify,
    Format,
    JsonExtract,
    VisionQa,
}

/// What the agent is allowed to DO to the filesystem and processes for a given
/// task. Modeled after Codex's three honest names. The path-jail (`workdir`
/// confinement) applies in WorkspaceWrite by default; `DangerFullAccess`
/// removes that restriction at the boundary tools enforce (today only path
/// resolution).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    /// Side-effect-free tools only. Writes / shell / patch / etc. are blocked
    /// with a clear error the agent can act on. Use for code review, audits,
    /// exploring a repo you don't trust to modify yet.
    ReadOnly,
    /// Default. Agent may write/run, but writes are confined to the task's
    /// workdir (or worktree). The current behavior.
    #[default]
    WorkspaceWrite,
    /// No restrictions beyond what individual tools impose. Use when the agent
    /// needs to touch files outside the workdir or run privileged commands.
    DangerFullAccess,
}

impl SandboxMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::DangerFullAccess => "danger_full_access",
        }
    }
}

impl std::fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SandboxMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "read_only" | "read-only" | "readonly" => Ok(Self::ReadOnly),
            "workspace_write" | "workspace-write" | "" => Ok(Self::WorkspaceWrite),
            "danger_full_access" | "danger-full-access" | "danger" => Ok(Self::DangerFullAccess),
            other => Err(format!("unknown sandbox mode: {other}")),
        }
    }
}
