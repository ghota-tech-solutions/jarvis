//! Built-in tool implementations: fs_read, fs_write, shell.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use jarvis_sandbox::SandboxSpec;
use serde::Deserialize;
use serde_json::{json, Value as Json};
use std::time::Duration;
use tokio::io::AsyncReadExt;

// ------------------ fs_read ------------------

#[derive(Debug, Default)]
pub struct FsReadTool;

#[derive(Debug, Deserialize)]
struct FsReadArgs {
    path: String,
    #[serde(default)]
    max_bytes: Option<usize>,
}

#[async_trait]
impl Tool for FsReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs_read".to_string(),
            description: "Read a UTF-8 text file (relative to the task workdir).".to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "path":      { "type": "string", "description": "file path, relative to workdir" },
                    "max_bytes": { "type": "integer", "description": "optional cap (default 256 KiB)" }
                },
                "required": ["path"]
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: FsReadArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = ctx.resolve(&a.path)?;
        let limit = a.max_bytes.unwrap_or(256 * 1024);
        let f = tokio::fs::File::open(&path).await?;
        let mut buf = Vec::with_capacity(8192);
        f.take(limit as u64).read_to_end(&mut buf).await?;
        let content = String::from_utf8_lossy(&buf).into_owned();
        let truncated = (buf.len() == limit) as i64;
        Ok(ToolOutput::ok(
            format!("read {} bytes from {}", buf.len(), path.display()),
            json!({ "content": content, "bytes": buf.len(), "truncated": truncated }),
        ))
    }
}

// ------------------ fs_write ------------------

#[derive(Debug, Default)]
pub struct FsWriteTool;

#[derive(Debug, Deserialize)]
struct FsWriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    create_dirs: bool,
}

#[async_trait]
impl Tool for FsWriteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs_write".to_string(),
            description:
                "Write (or overwrite) a UTF-8 text file relative to workdir. Returns bytes written."
                    .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "path":        { "type": "string" },
                    "content":     { "type": "string" },
                    "create_dirs": { "type": "boolean", "description": "create parent dirs if missing (default false)" }
                },
                "required": ["path", "content"]
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: FsWriteArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = ctx.resolve(&a.path)?;
        if a.create_dirs
            && let Some(parent) = path.parent()
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, &a.content).await?;
        Ok(ToolOutput::ok(
            format!("wrote {} bytes to {}", a.content.len(), path.display()),
            json!({ "bytes": a.content.len() }),
        ))
    }
}

// ------------------ shell ------------------

#[derive(Debug, Default)]
pub struct ShellTool;

#[derive(Debug, Deserialize)]
struct ShellArgs {
    cmd: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_timeout() -> u64 {
    60
}

#[async_trait]
impl Tool for ShellTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "shell".to_string(),
            description:
                "Run a shell command inside the task's sandbox (native or docker), in workdir. \
                 Returns exit_code, stdout, stderr."
                    .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "cmd":          { "type": "string" },
                    "timeout_secs": { "type": "integer", "description": "default 60" }
                },
                "required": ["cmd"]
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: ShellArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let spec = SandboxSpec::new(&a.cmd, &ctx.workdir)
            .with_timeout(Duration::from_secs(a.timeout_secs))
            .with_net(ctx.net_policy.clone());

        let exec_fut = ctx.sandbox.exec(spec);
        let output = tokio::select! {
            res = exec_fut => res,
            _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
        };
        let output = output.map_err(|e| ToolError::Other(e.to_string()))?;

        let is_error = output.exit_code != 0 || output.timed_out;
        let mut out = ToolOutput::ok(
            format!(
                "[{}] `{}` -> exit={}{}",
                output.backend,
                clip(&a.cmd, 80),
                output.exit_code,
                if output.timed_out { " (timeout)" } else { "" },
            ),
            json!({
                "backend": output.backend,
                "exit_code": output.exit_code,
                "stdout": clip(&output.stdout, 16 * 1024),
                "stderr": clip(&output.stderr, 16 * 1024),
                "stdout_bytes": output.stdout.len(),
                "stderr_bytes": output.stderr.len(),
                "timed_out": output.timed_out,
            }),
        );
        out.is_error = is_error;
        Ok(out)
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…[{}b truncated]", &s[..max], s.len() - max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn fs_write_then_read_roundtrip() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let w = FsWriteTool;
        w.invoke(
            json!({"path": "hello.txt", "content": "salut"}),
            &ctx,
        )
        .await
        .unwrap();
        let r = FsReadTool;
        let out = r.invoke(json!({"path": "hello.txt"}), &ctx).await.unwrap();
        assert_eq!(out.data["content"], "salut");
    }

    #[tokio::test]
    async fn fs_read_rejects_escape() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let r = FsReadTool;
        let err = r.invoke(json!({"path": "../escape"}), &ctx).await.err();
        assert!(matches!(err, Some(ToolError::PathEscapes { .. })));
    }

    #[tokio::test]
    async fn shell_captures_exit_and_stdout() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let s = ShellTool;
        let out = s.invoke(json!({"cmd": "echo hi"}), &ctx).await.unwrap();
        assert!(!out.is_error);
        assert!(out.data["stdout"].as_str().unwrap().contains("hi"));
        assert_eq!(out.data["backend"], "native");
    }
}
