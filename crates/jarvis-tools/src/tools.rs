//! Built-in tool implementations: fs_read, fs_write, shell.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use jarvis_sandbox::SandboxSpec;
use serde::Deserialize;
use serde_json::{Value as Json, json};
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
    /// 1-based starting line, inclusive. Defaults to 1.
    #[serde(default)]
    start_line: Option<usize>,
    /// 1-based ending line, inclusive. Defaults to the end of the file.
    #[serde(default)]
    end_line: Option<usize>,
}

/// Cap on the returned slice when the agent didn't specify a range. Beyond this
/// we return a head window and append a hint nudging the agent to call again
/// with start_line/end_line. Keeps the local-model context cheap on huge files.
const FS_READ_DEFAULT_LINES: usize = 200;

#[async_trait]
impl Tool for FsReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs_read".to_string(),
            description: "Read a UTF-8 text file (relative to the task workdir). For files over \
                 ~200 lines, ALWAYS pass `start_line`/`end_line` instead of reading the whole \
                 file — full reads bloat the conversation context and slow the model down. \
                 If you only need to edit something, prefer `apply_patch` with an `@@ anchor` \
                 (a unique substring of the target line); apply_patch does NOT require a \
                 prior fs_read."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "path":       { "type": "string",  "description": "file path, relative to workdir" },
                    "start_line": { "type": "integer", "description": "1-based first line to return (inclusive)" },
                    "end_line":   { "type": "integer", "description": "1-based last line to return (inclusive)" },
                    "max_bytes":  { "type": "integer", "description": "byte cap (default 256 KiB)" }
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
        let bytes_read = buf.len();
        let raw = String::from_utf8_lossy(&buf).into_owned();
        let total_lines = raw.lines().count();
        let byte_truncated = bytes_read == limit;

        // Slice by line range. If neither bound was provided AND the file is
        // big, default to the first FS_READ_DEFAULT_LINES so we don't dump
        // thousands of lines into the model's context by accident.
        let explicit_range = a.start_line.is_some() || a.end_line.is_some();
        let start = a.start_line.unwrap_or(1).max(1);
        let mut end = a.end_line.unwrap_or(total_lines);
        if !explicit_range && total_lines > FS_READ_DEFAULT_LINES {
            end = FS_READ_DEFAULT_LINES;
        }
        let end = end.min(total_lines);

        let mut content = String::new();
        if start <= end {
            for (i, line) in raw.lines().enumerate() {
                let lineno = i + 1;
                if lineno < start {
                    continue;
                }
                if lineno > end {
                    break;
                }
                content.push_str(line);
                content.push('\n');
            }
        }
        // Preserve the trailing-newline truth of the original on full-file reads.
        if start == 1 && end == total_lines && !raw.ends_with('\n') {
            content.pop();
        }

        let range_truncated = end < total_lines;
        // Only nag the agent when we silently shortened the read — if the agent
        // explicitly asked for lines 10-12, the missing 13-50 is intentional.
        let hint = if range_truncated && !explicit_range {
            format!(
                "\n[truncated: showing lines {start}-{end} of {total_lines}. \
                 Re-call fs_read with explicit start_line/end_line for more.]\n"
            )
        } else {
            String::new()
        };
        let display_content = if hint.is_empty() {
            content.clone()
        } else {
            format!("{content}{hint}")
        };

        let summary = if range_truncated {
            format!(
                "read lines {start}-{end} of {total_lines} ({bytes_read} bytes scanned) from {}",
                path.display()
            )
        } else {
            format!("read {bytes_read} bytes from {}", path.display())
        };

        Ok(ToolOutput::ok(
            summary,
            json!({
                "content": display_content,
                "bytes": bytes_read,
                "start_line": start,
                "end_line": end,
                "total_lines": total_lines,
                "truncated": (byte_truncated || range_truncated) as i64,
            }),
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
                "Write (or overwrite) a UTF-8 text file relative to workdir. Returns a diff \
                 summary (lines_added, lines_removed, is_new) plus a capped unified diff."
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

        // Read the existing content (if any) before overwriting so we can emit a diff.
        let (existed, old_content) = match tokio::fs::read_to_string(&path).await {
            Ok(s) => (true, s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (false, String::new()),
            Err(e) => return Err(ToolError::Io(e)),
        };

        tokio::fs::write(&path, &a.content).await?;

        let diff = compute_diff(&old_content, &a.content);
        let display_path = path.display().to_string();
        Ok(ToolOutput::ok(
            if existed {
                format!(
                    "edited {} (+{} -{})",
                    display_path, diff.lines_added, diff.lines_removed
                )
            } else {
                format!("created {} (+{} lines)", display_path, diff.lines_added)
            },
            json!({
                "path": display_path,
                "bytes": a.content.len(),
                "is_new": !existed,
                "lines_added": diff.lines_added,
                "lines_removed": diff.lines_removed,
                "diff_unified": diff.unified,
                "diff_truncated": diff.truncated,
            }),
        ))
    }
}

struct DiffSummary {
    lines_added: usize,
    lines_removed: usize,
    unified: String,
    truncated: bool,
}

fn compute_diff(old: &str, new: &str) -> DiffSummary {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let mut lines_added = 0;
    let mut lines_removed = 0;
    let mut unified = String::new();
    let mut truncated = false;
    const MAX_LINES: usize = 400;
    let mut emitted = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => lines_added += 1,
            ChangeTag::Delete => lines_removed += 1,
            ChangeTag::Equal => {}
        }
        if emitted < MAX_LINES {
            let sign = match change.tag() {
                ChangeTag::Insert => "+",
                ChangeTag::Delete => "-",
                ChangeTag::Equal => " ",
            };
            unified.push_str(sign);
            unified.push_str(change.as_str().unwrap_or(""));
            if !change.as_str().unwrap_or("").ends_with('\n') {
                unified.push('\n');
            }
            emitted += 1;
        } else if !truncated {
            unified.push_str("…[truncated]\n");
            truncated = true;
        }
    }
    DiffSummary {
        lines_added,
        lines_removed,
        unified,
        truncated,
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
                jarvis_core::clip(&a.cmd, 80),
                output.exit_code,
                if output.timed_out { " (timeout)" } else { "" },
            ),
            json!({
                "backend": output.backend,
                "exit_code": output.exit_code,
                "stdout": jarvis_core::clip(&output.stdout, 16 * 1024),
                "stderr": jarvis_core::clip(&output.stderr, 16 * 1024),
                "stdout_bytes": output.stdout.len(),
                "stderr_bytes": output.stderr.len(),
                "timed_out": output.timed_out,
            }),
        );
        out.is_error = is_error;
        Ok(out)
    }
}

// ------------------ update_plan ------------------

#[derive(Debug, Default)]
pub struct UpdatePlanTool;

#[derive(Debug, Deserialize)]
struct UpdatePlanArgs {
    /// Ordered checklist. At most one step may be `in_progress` at a time.
    plan: Vec<PlanStep>,
    /// Optional short note explaining the update (e.g. "split step 3").
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize, serde::Serialize)]
struct PlanStep {
    step: String,
    /// `pending` | `in_progress` | `completed`.
    status: String,
}

/// Coerce a sloppy `plan` argument into the canonical array shape.
///
/// Gemma 4 mangles `update_plan` in two observed ways (task 29fa7542):
/// it sends `plan` as a JSON-encoded *string* rather than an array, and it
/// sometimes wraps the real array under a second `plan` key
/// (`{"plan":{"plan":[…]}}`). Both are recoverable without losing intent.
fn coerce_plan_args(mut args: Json) -> Json {
    let Some(obj) = args.as_object_mut() else {
        return args;
    };
    // 1) String-encoded plan: `"plan": "[…]"` or `"plan": "{\"plan\":[…]}"`.
    if let Some(Json::String(s)) = obj.get("plan")
        && let Ok(parsed) = serde_json::from_str::<Json>(s)
    {
        obj.insert("plan".into(), parsed);
    }
    // 2) Double-wrapped plan: `"plan": {"plan": [...]}` → unwrap one level.
    let unwrapped = match obj.get("plan") {
        Some(Json::Object(inner)) if inner.len() == 1 => {
            inner.get("plan").filter(|v| v.is_array()).cloned()
        }
        _ => None,
    };
    if let Some(arr) = unwrapped {
        obj.insert("plan".into(), arr);
    }
    args
}

#[async_trait]
impl Tool for UpdatePlanTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "update_plan".to_string(),
            description:
                "Record or update the task plan as a checklist of steps. The harness renders \
                 the plan separately (sidebar + a single system message echoing the current \
                 state), so you DO NOT have to restate the plan in your `thought` or `message` \
                 — that would just waste tokens. Call this when scope is decided, when scope \
                 changes, and to advance status as you work.\n\
                 Rules: at most ONE step may be `in_progress` at any time. Steps stay short \
                 (imperative, < 80 chars). Use `pending` | `in_progress` | `completed`."
                    .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step":   { "type": "string" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                            },
                            "required": ["step", "status"]
                        }
                    },
                    "note": { "type": "string", "description": "optional short reason for this update" }
                },
                "required": ["plan"]
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args = coerce_plan_args(args);
        let a: UpdatePlanArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            // Soft error, not a hard `ToolError`: the model gets the result
            // back and can retry on the next step with the right shape,
            // instead of the loop swallowing an opaque parse failure.
            Err(e) => {
                return Ok(ToolOutput::err(
                    format!(
                        "invalid plan args ({e}). Pass `plan` as an array of \
                         {{\"step\",\"status\"}} objects inside `args`, e.g. \
                         args={{\"plan\":[{{\"step\":\"Read README\",\"status\":\"completed\"}},\
                         {{\"step\":\"Audit core crate\",\"status\":\"in_progress\"}}]}}"
                    ),
                    json!({ "error": "invalid_args" }),
                ));
            }
        };
        let n = a.plan.len();
        let in_progress = a.plan.iter().filter(|s| s.status == "in_progress").count();
        if in_progress > 1 {
            return Ok(ToolOutput::err(
                format!("invalid plan: {in_progress} steps in_progress (max 1)"),
                json!({ "error": "multiple_in_progress" }),
            ));
        }
        let done = a.plan.iter().filter(|s| s.status == "completed").count();
        let summary = format!("plan updated — {done}/{n} done");
        Ok(ToolOutput::ok(
            summary,
            json!({
                "plan": a.plan,
                "note": a.note,
            }),
        ))
    }
}

// ------------------ apply_patch ------------------

#[derive(Debug, Default)]
pub struct ApplyPatchTool;

#[derive(Debug, Deserialize)]
struct ApplyPatchArgs {
    input: String,
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "apply_patch".to_string(),
            description:
                "Apply a Codex-style patch envelope. Prefer this over `fs_write` for edits — it \
                 only sends the changed lines (with 1–3 context lines and an optional anchor) \
                 instead of the full file, which is dramatically cheaper in tokens.\n\
                 Format:\n\
                 ```\n\
                 *** Begin Patch\n\
                 *** Update File: relative/path.rs\n\
                 @@ optional_anchor (a substring that appears in the target line)\n\
                  unchanged context line\n\
                 -removed line\n\
                 +added line\n\
                  unchanged context line\n\
                 *** End Patch\n\
                 ```\n\
                 Also supports `*** Add File: <path>` (then `+` lines for new content) and \
                 `*** Delete File: <path>`. Inside an Update block, `*** Move to: <new_path>` \
                 renames the file. Paths are relative to the workdir."
                    .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string", "description": "the full patch envelope text" }
                },
                "required": ["input"]
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: ApplyPatchArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let ops = match crate::apply_patch::parse(&a.input) {
            Ok(o) => o,
            Err(e) => {
                return Ok(ToolOutput::err(
                    format!("parse error: {e}"),
                    json!({ "error": e.to_string() }),
                ));
            }
        };
        let workdir = ctx.workdir.clone();
        // The applicator does blocking std::fs work — keep it off the async runtime.
        let report =
            match tokio::task::spawn_blocking(move || crate::apply_patch::apply(&ops, &workdir))
                .await
            {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    return Ok(ToolOutput::err(
                        format!("apply error: {e}"),
                        json!({ "error": e.to_string() }),
                    ));
                }
                Err(e) => return Err(ToolError::Other(format!("join: {e}"))),
            };

        let total_add: usize = report.files.iter().map(|f| f.lines_added).sum();
        let total_rem: usize = report.files.iter().map(|f| f.lines_removed).sum();
        let summary = if report.files.len() == 1 {
            format!(
                "{} {} (+{} -{})",
                report.files[0].status.as_str(),
                report.files[0].path,
                report.files[0].lines_added,
                report.files[0].lines_removed
            )
        } else {
            format!(
                "{} files changed (+{} -{})",
                report.files.len(),
                total_add,
                total_rem
            )
        };
        let files_json: Vec<_> = report
            .files
            .iter()
            .map(|f| {
                json!({
                    "path": f.path,
                    "status": f.status.as_str(),
                    "lines_added": f.lines_added,
                    "lines_removed": f.lines_removed,
                    "diff_unified": f.diff_unified,
                    "diff_truncated": f.diff_truncated,
                })
            })
            .collect();
        Ok(ToolOutput::ok(
            summary,
            json!({
                "files": files_json,
                "lines_added": total_add,
                "lines_removed": total_rem,
            }),
        ))
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
        let out = w
            .invoke(json!({"path": "hello.txt", "content": "salut"}), &ctx)
            .await
            .unwrap();
        assert_eq!(out.data["is_new"], true);
        let r = FsReadTool;
        let out = r.invoke(json!({"path": "hello.txt"}), &ctx).await.unwrap();
        assert_eq!(out.data["content"], "salut");
    }

    #[tokio::test]
    async fn fs_write_diff_on_overwrite() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let w = FsWriteTool;
        w.invoke(
            json!({"path":"f.txt","content":"line1\nline2\nline3\n"}),
            &ctx,
        )
        .await
        .unwrap();
        let out = w
            .invoke(
                json!({"path":"f.txt","content":"line1\nLINE-TWO\nline3\nextra\n"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.data["is_new"], false);
        assert_eq!(out.data["lines_added"].as_u64().unwrap(), 2);
        assert_eq!(out.data["lines_removed"].as_u64().unwrap(), 1);
        let unified = out.data["diff_unified"].as_str().unwrap();
        assert!(unified.contains("+LINE-TWO"));
        assert!(unified.contains("-line2"));
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
    async fn fs_read_default_truncates_huge_files() {
        // Files past FS_READ_DEFAULT_LINES (200) should be sliced to the first
        // window when the agent didn't ask for a specific range — protects the
        // local model's context from an accidental 13 KB README dump.
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let big: String = (1..=400).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();
        let out = FsReadTool
            .invoke(json!({"path": "big.txt"}), &ctx)
            .await
            .unwrap();
        assert_eq!(out.data["start_line"], 1);
        assert_eq!(out.data["end_line"], 200);
        assert_eq!(out.data["total_lines"], 400);
        assert_eq!(out.data["truncated"], 1);
        let content = out.data["content"].as_str().unwrap();
        assert!(content.contains("line 1\n"));
        assert!(content.contains("line 200\n"));
        assert!(!content.contains("line 201"));
        assert!(content.contains("[truncated: showing lines 1-200 of 400"));
    }

    #[tokio::test]
    async fn fs_read_honors_explicit_range() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let body: String = (1..=50).map(|i| format!("L{i}\n")).collect();
        std::fs::write(dir.path().join("file.txt"), &body).unwrap();
        let out = FsReadTool
            .invoke(
                json!({"path": "file.txt", "start_line": 10, "end_line": 12}),
                &ctx,
            )
            .await
            .unwrap();
        let content = out.data["content"].as_str().unwrap();
        assert_eq!(content, "L10\nL11\nL12\n");
        assert_eq!(out.data["start_line"], 10);
        assert_eq!(out.data["end_line"], 12);
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

    #[tokio::test]
    async fn update_plan_accepts_canonical_array() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let out = UpdatePlanTool
            .invoke(
                json!({"plan":[
                    {"step":"a","status":"completed"},
                    {"step":"b","status":"in_progress"}
                ]}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.data["plan"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn update_plan_coerces_stringified_plan() {
        // Task 29fa7542 step 7: `plan` arrived as a JSON-encoded string that
        // itself wraps the array under a second `plan` key.
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let out = UpdatePlanTool
            .invoke(
                json!({"plan": "{\"plan\":[{\"step\":\"a\",\"status\":\"completed\"}]}"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.data["plan"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn update_plan_unwraps_double_wrapped_plan() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let out = UpdatePlanTool
            .invoke(
                json!({"plan": {"plan": [{"step":"a","status":"pending"}]}}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.data["plan"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn update_plan_missing_plan_is_soft_error() {
        // An unrecoverable call returns a soft error (the model retries next
        // step) rather than a hard `ToolError`.
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let out = UpdatePlanTool.invoke(json!({}), &ctx).await.unwrap();
        assert!(out.is_error);
        assert_eq!(out.data["error"], "invalid_args");
    }
}
