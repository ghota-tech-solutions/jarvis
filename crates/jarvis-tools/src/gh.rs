//! GitHub integration via the `gh` CLI.
//!
//! Why passthrough instead of the GitHub REST API:
//! - `gh` already handles the user's OAuth token (`gh auth login`), token
//!   refresh, multi-account, GitHub Enterprise — none of which we want to
//!   reimplement.
//! - Output is JSON-by-flag (`--json`), so the agent gets structured data
//!   without us shipping a Rust GitHub client.
//! - Multi-user / multi-repo "review across your fleet" usage works
//!   automatically.
//!
//! Tools: `gh.pr_list`, `gh.pr_view`, `gh.pr_comment`, `gh.pr_create`.
//! All run via the configured sandbox so net policy + workdir confinement
//! apply uniformly. Errors are surfaced as `is_error: true` so the agent
//! sees them as failed tool calls rather than panicking the loop.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use jarvis_sandbox::SandboxSpec;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::time::Duration;

const DEFAULT_TIMEOUT_S: u64 = 30;

async fn run_gh(ctx: &ToolCtx, cmd: String) -> Result<ToolOutput, ToolError> {
    let spec = SandboxSpec {
        cmd: cmd.clone(),
        workdir: ctx.workdir.clone(),
        env: std::collections::HashMap::new(),
        timeout: Duration::from_secs(DEFAULT_TIMEOUT_S),
        net: ctx.net_policy.clone(),
    };
    let out = ctx
        .sandbox
        .exec(spec)
        .await
        .map_err(|e| ToolError::Other(format!("sandbox: {e}")))?;
    if out.timed_out {
        return Ok(ToolOutput::err(
            format!("gh timed out after {DEFAULT_TIMEOUT_S}s"),
            json!({"cmd": cmd, "stdout": out.stdout, "stderr": out.stderr}),
        ));
    }
    if out.exit_code != 0 {
        return Ok(ToolOutput::err(
            format!("gh exit {} — {}", out.exit_code, first_line(&out.stderr)),
            json!({
                "cmd": cmd,
                "exit_code": out.exit_code,
                "stdout": out.stdout,
                "stderr": out.stderr,
            }),
        ));
    }
    // Try to surface stdout as JSON if possible so the agent gets typed
    // data; else fall through to a raw string.
    let parsed: Option<Json> = serde_json::from_str(out.stdout.trim()).ok();
    Ok(ToolOutput::ok(
        format!("gh ok ({} bytes)", out.stdout.len()),
        json!({
            "cmd": cmd,
            "data": parsed,
            "stdout": out.stdout,
        }),
    ))
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(160).collect()
}

fn shell_arg(s: &str) -> String {
    // Single-quote the argument and escape any embedded single quote with the
    // canonical `'\''` trick. Safe for bash, cmd.exe (via /S /C), and
    // PowerShell (we never use single-quote-stripping there).
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

// ---------- gh.pr_list ----------

pub struct GhPrListTool;

#[derive(Debug, Deserialize)]
struct PrListArgs {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[async_trait]
impl Tool for GhPrListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "gh.pr_list".to_string(),
            description: "List open pull requests in the current repo via the `gh` CLI. \
                Returns parsed JSON [{number, title, author, headRefName, url, ...}]. \
                State defaults to 'open'; pass 'all' or 'closed' to widen."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "state": { "type": "string", "enum": ["open", "closed", "merged", "all"] },
                    "limit": { "type": "integer", "description": "max PRs to return (default 20)" }
                }
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: PrListArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let state = a.state.unwrap_or_else(|| "open".to_string());
        let limit = a.limit.unwrap_or(20).clamp(1, 100);
        let cmd = format!(
            "gh pr list --state {} --limit {} --json number,title,author,headRefName,baseRefName,url,createdAt,updatedAt,isDraft,labels",
            shell_arg(&state),
            limit,
        );
        run_gh(ctx, cmd).await
    }
}

// ---------- gh.pr_view ----------

pub struct GhPrViewTool;

#[derive(Debug, Deserialize)]
struct PrViewArgs {
    /// PR number (e.g. 42) OR url ("https://github.com/o/r/pull/42").
    pr: String,
    /// If true, includes the diff. Big payloads — costs context.
    #[serde(default)]
    with_diff: Option<bool>,
}

#[async_trait]
impl Tool for GhPrViewTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "gh.pr_view".to_string(),
            description: "Fetch a single PR's details + comments + reviews via `gh`. \
                Pass `with_diff: true` to also include the unified diff (large)."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "pr":        { "type": "string",  "description": "PR number or full URL" },
                    "with_diff": { "type": "boolean", "description": "include unified diff (default false)" }
                },
                "required": ["pr"]
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: PrViewArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let with_diff = a.with_diff.unwrap_or(false);
        let cmd = if with_diff {
            format!(
                "gh pr view {} --json number,title,body,author,headRefName,baseRefName,url,state,isDraft,reviews,comments,files,additions,deletions && gh pr diff {}",
                shell_arg(&a.pr),
                shell_arg(&a.pr),
            )
        } else {
            format!(
                "gh pr view {} --json number,title,body,author,headRefName,baseRefName,url,state,isDraft,reviews,comments,files,additions,deletions",
                shell_arg(&a.pr),
            )
        };
        run_gh(ctx, cmd).await
    }
}

// ---------- gh.pr_comment ----------

pub struct GhPrCommentTool;

#[derive(Debug, Deserialize)]
struct PrCommentArgs {
    pr: String,
    body: String,
}

#[async_trait]
impl Tool for GhPrCommentTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "gh.pr_comment".to_string(),
            description: "Post a top-level comment on a PR via `gh`. \
                For per-line review comments use `gh pr review` instead — this \
                tool intentionally stays simple."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "pr":   { "type": "string", "description": "PR number or URL" },
                    "body": { "type": "string", "description": "markdown comment body" }
                },
                "required": ["pr", "body"]
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: PrCommentArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        if a.body.trim().is_empty() {
            return Err(ToolError::InvalidArgs("body is empty".into()));
        }
        let cmd = format!(
            "gh pr comment {} --body {}",
            shell_arg(&a.pr),
            shell_arg(&a.body),
        );
        run_gh(ctx, cmd).await
    }
}

// ---------- gh.pr_create ----------

pub struct GhPrCreateTool;

#[derive(Debug, Deserialize)]
struct PrCreateArgs {
    title: String,
    #[serde(default)]
    body: Option<String>,
    /// Empty = let gh infer from current branch + default base.
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    draft: Option<bool>,
}

#[async_trait]
impl Tool for GhPrCreateTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "gh.pr_create".to_string(),
            description: "Open a new PR on the current branch via `gh pr create`. \
                Requires the branch to be pushed to the remote first."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "body":  { "type": "string", "description": "markdown body" },
                    "base":  { "type": "string", "description": "base branch (default = repo default)" },
                    "draft": { "type": "boolean" }
                },
                "required": ["title"]
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: PrCreateArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let title = a.title.trim();
        if title.is_empty() {
            return Err(ToolError::InvalidArgs("title is empty".into()));
        }
        let mut cmd = format!("gh pr create --title {}", shell_arg(title));
        cmd.push_str(&format!(
            " --body {}",
            shell_arg(a.body.as_deref().unwrap_or("(no body)")),
        ));
        if let Some(b) = a.base.as_deref().filter(|s| !s.is_empty()) {
            cmd.push_str(&format!(" --base {}", shell_arg(b)));
        }
        if a.draft.unwrap_or(false) {
            cmd.push_str(" --draft");
        }
        run_gh(ctx, cmd).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_arg_escapes_quotes() {
        assert_eq!(shell_arg("simple"), "'simple'");
        assert_eq!(shell_arg("a'b"), "'a'\\''b'");
        assert_eq!(shell_arg("multi\nline"), "'multi\nline'");
    }

    #[test]
    fn first_line_caps_length() {
        let long = "x".repeat(500);
        assert_eq!(first_line(&long).len(), 160);
        assert_eq!(first_line("a\nb"), "a");
        assert_eq!(first_line(""), "");
    }
}
