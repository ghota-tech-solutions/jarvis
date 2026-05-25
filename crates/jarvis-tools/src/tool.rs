use async_trait::async_trait;
use jarvis_core::LlmProvider;
use jarvis_sandbox::{NativeSandbox, NetPolicy, Sandbox};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Static description of a tool. Used both for the system prompt the LLM sees
/// and (later) to validate incoming `args`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON schema (object) describing the `args` payload.
    pub args_schema: Json,
    /// True if the tool can modify state (filesystem, network, processes).
    pub side_effects: bool,
}

/// Per-call context: the working directory the agent should be confined to,
/// the sandbox backend that executes side-effectful tools, optional cancellation, etc.
#[derive(Clone)]
pub struct ToolCtx {
    pub workdir: PathBuf,
    pub cancel: Arc<tokio_util::sync::CancellationToken>,
    pub sandbox: Arc<dyn Sandbox>,
    /// Network policy passed to the sandbox on shell calls.
    pub net_policy: NetPolicy,
    /// M11.S3: id (as UUID string) of the task whose agent loop is invoking
    /// this tool. Used by `SpawnSubagentTool` to attribute children to the
    /// right parent in the FleetDag. Empty for unit-test contexts.
    pub current_task_id: String,
    /// § T2.3 — Architect/Editor pipeline. When the daemon's `[routing]
    /// editor_model` is configured, this carries a provider locked to that
    /// model so tools like `request_edit` can delegate patch-generation
    /// to a cheap local model while the main agent loop stays on a
    /// stronger remote architect. `None` when the pipeline is disabled —
    /// the request_edit tool then errors out with a clear message and
    /// the agent falls back to `apply_patch` directly.
    pub editor: Option<Arc<dyn LlmProvider>>,
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("workdir", &self.workdir)
            .field("net_policy", &self.net_policy)
            .field("sandbox", &self.sandbox.kind())
            .finish()
    }
}

impl ToolCtx {
    /// Convenience: a context with the Native backend, full network. Use real
    /// `ToolCtx { ... }` construction in the daemon to wire a Docker backend.
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
            cancel: Arc::new(tokio_util::sync::CancellationToken::new()),
            sandbox: Arc::new(NativeSandbox),
            net_policy: NetPolicy::Full,
            current_task_id: String::new(),
            editor: None,
        }
    }

    /// Resolve a user-provided path against `workdir` and reject any path that
    /// escapes the workdir (after canonicalization-free joining).
    pub fn resolve(&self, p: &str) -> Result<PathBuf, ToolError> {
        let p = Path::new(p);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.workdir.join(p)
        };
        // Lexical confinement check (no canonicalize because the file may not exist yet).
        if !path_within(&joined, &self.workdir) {
            return Err(ToolError::PathEscapes {
                path: joined.display().to_string(),
                root: self.workdir.display().to_string(),
            });
        }
        Ok(joined)
    }
}

fn path_within(p: &Path, root: &Path) -> bool {
    // Reject any `..` segments that would walk above root.
    let mut depth: i32 = 0;
    for comp in p.strip_prefix(root).unwrap_or(p).components() {
        match comp {
            std::path::Component::ParentDir => depth -= 1,
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                // Absolute path inside or outside root — must literally be inside root.
                return p.starts_with(root);
            }
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

/// What a tool returns. Free-form JSON `data` for typed callers,
/// plus a short `summary` for the ledger UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub summary: String,
    pub data: Json,
    /// If true, the agent should treat this as an unrecoverable error for this step.
    #[serde(default)]
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(summary: impl Into<String>, data: Json) -> Self {
        Self {
            summary: summary.into(),
            data,
            is_error: false,
        }
    }
    pub fn err(summary: impl Into<String>, data: Json) -> Self {
        Self {
            summary: summary.into(),
            data,
            is_error: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid args: {0}")]
    InvalidArgs(String),
    #[error("path `{path}` escapes workdir `{root}`")]
    PathEscapes { path: String, root: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_keeps_relative_inside() {
        let ctx = ToolCtx::new("/tmp/workdir");
        assert!(ctx.resolve("src/main.rs").is_ok());
        assert!(ctx.resolve("./README.md").is_ok());
    }

    #[test]
    fn resolve_rejects_parent_escape() {
        let ctx = ToolCtx::new("/tmp/workdir");
        assert!(ctx.resolve("../etc/passwd").is_err());
        assert!(ctx.resolve("src/../../etc/passwd").is_err());
    }
}
