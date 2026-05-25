// § T2.3 v1 — Architect/Editor pipeline.
//
// `request_edit(path, intent)` is the architect-friendly companion to
// `apply_patch`. Instead of asking the architect (a strong, expensive
// remote model) to produce a precise unified-diff envelope itself, the
// architect just describes the *intent* in prose ("rename foo to bar
// in src/lib.rs and update its single call-site"). This tool:
//
//   1. Reads the target file.
//   2. Calls the configured editor LLM (cheap local model — typically
//      a Gemma / Qwen 4B-7B) with the file contents + the intent.
//   3. Parses the editor's reply as an `apply_patch` envelope.
//   4. Delegates execution to the existing `ApplyPatchTool`.
//
// The editor LLM is the one configured via `[routing] editor_model`
// in jarvis.toml and threaded through `ToolCtx.editor`. When no
// editor is configured the tool fails fast with a clear message so
// the agent falls back to `apply_patch` directly.

use crate::ApplyPatchTool;
use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use jarvis_core::{ChatMessage, ChatRequest};
use serde_json::{Value as Json, json};
use std::fs;

const MAX_FILE_BYTES: usize = 64 * 1024;
const EDITOR_MAX_TOKENS: u32 = 2048;

pub struct RequestEditTool;

#[async_trait]
impl Tool for RequestEditTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "request_edit".to_string(),
            description:
                "Delegate a code edit to the cheap local editor model. Describe the change as prose intent — the editor reads the file and produces the apply_patch envelope. Use this instead of `apply_patch` when the daemon has `[routing] editor_model` configured: the architect (you) stays focused on planning, the editor handles the mechanical patch generation. Falls back to a clear error if no editor is wired."
                    .to_string(),
            args_schema: json!({
                "type": "object",
                "required": ["path", "intent"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit, relative to the workdir."
                    },
                    "intent": {
                        "type": "string",
                        "description": "Prose description of the change. Be specific about WHAT changes (rename X to Y, add a JWT middleware before route Z, fix the off-by-one in line N) — the editor will translate to exact +/- lines."
                    }
                }
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let editor = ctx.editor.as_ref().ok_or_else(|| {
            ToolError::InvalidArgs(
                "request_edit requires `[routing] editor_model` in jarvis.toml; \
                 fall back to `apply_patch` instead."
                    .to_string(),
            )
        })?;
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("path is required".to_string()))?;
        let intent = args
            .get("intent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("intent is required".to_string()))?;
        let intent = intent.trim();
        if intent.is_empty() {
            return Err(ToolError::InvalidArgs("intent is empty".to_string()));
        }

        // Read the target file. Confined to the workdir like every fs
        // operation — we reuse the resolver to enforce the same check.
        let resolved = ctx.resolve(path)?;
        let raw = fs::read_to_string(&resolved).map_err(ToolError::Io)?;
        if raw.len() > MAX_FILE_BYTES {
            return Err(ToolError::InvalidArgs(format!(
                "file is too large ({} bytes); the editor pipeline caps inputs at {} \
                 bytes. Use `apply_patch` directly for big files.",
                raw.len(),
                MAX_FILE_BYTES
            )));
        }

        let envelope = call_editor(editor.as_ref(), path, &raw, intent)
            .await
            .map_err(|e| ToolError::Other(format!("editor call failed: {e}")))?;

        // Delegate to apply_patch with the editor's envelope.
        let apply = ApplyPatchTool;
        let apply_args = json!({ "patch": envelope });
        let out = apply.invoke(apply_args, ctx).await?;
        Ok(ToolOutput {
            summary: format!("editor patched `{path}` ({} bytes)", raw.len()),
            data: json!({
                "path": path,
                "intent": intent,
                "envelope": envelope,
                "apply_result": out.data,
                "editor_summary": out.summary,
            }),
            is_error: out.is_error,
        })
    }
}

async fn call_editor(
    editor: &dyn jarvis_core::LlmProvider,
    path: &str,
    file: &str,
    intent: &str,
) -> jarvis_core::Result<String> {
    let system = r#"
You are the EDITOR half of an architect/editor pipeline for an
autonomous coding agent. The architect just decided "edit this file
with this intent" — your one job is to produce a precise `apply_patch`
envelope that fulfils the intent. Output ONLY the envelope, nothing
else. No prose, no markdown fences.

Envelope format (verbatim):

*** Begin Patch
*** Update File: <path>
@@ <anchor — a unique substring of a line near the change>
 <context line, prefix space>
-<line to remove, prefix minus>
+<line to add, prefix plus>
 <context line>
*** End Patch

Rules:
- Use the EXACT path the architect gave you.
- Include 1–3 unchanged context lines around each hunk.
- The `@@ anchor` is OPTIONAL but helpful for ambiguous changes.
- Multiple hunks per file are fine — just stack them.
- Do NOT include the file content beyond what's needed; the patch is
  the minimal diff.
- If the intent is impossible or ambiguous given the file content,
  output ONLY a single line `// EDITOR_REFUSED: <reason>` and no
  envelope.
"#;
    let user = format!(
        "Path: {path}\n\nIntent:\n{intent}\n\nCurrent file content:\n```\n{file}\n```\n\nProduce the apply_patch envelope now."
    );
    let req = ChatRequest {
        messages: vec![
            ChatMessage::system(system.trim().to_string()),
            ChatMessage::user(user),
        ],
        temperature: Some(0.0),
        top_p: None,
        max_tokens: Some(EDITOR_MAX_TOKENS),
        stream: false,
    };
    let resp = editor.complete(req).await?;
    let body = resp.content.trim();
    if let Some(reason) = body.strip_prefix("// EDITOR_REFUSED:") {
        return Err(jarvis_core::Error::Provider(format!(
            "editor refused: {}",
            reason.trim()
        )));
    }
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_required_fields() {
        let s = RequestEditTool.schema();
        assert_eq!(s.name, "request_edit");
        assert!(s.side_effects);
        let req = s.args_schema["required"].as_array().unwrap();
        let names: Vec<&str> = req.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"path"));
        assert!(names.contains(&"intent"));
    }

    #[tokio::test]
    async fn errors_without_editor_in_ctx() {
        let ctx = ToolCtx::new(std::env::temp_dir());
        let out = RequestEditTool
            .invoke(json!({ "path": "x.rs", "intent": "do a thing" }), &ctx)
            .await;
        match out {
            Err(ToolError::InvalidArgs(msg)) => {
                assert!(msg.contains("editor_model"));
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }
}
