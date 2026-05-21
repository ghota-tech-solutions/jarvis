//! Surgical Search-and-Replace File Editor.
//!
//! Exposes a highly reliable, token-efficient surgical edit tool
//! that allows agents to replace exact, unique blocks of text in a file
//! without rewriting the entire file or generating complex, fragile diffs.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value as Json, json};

#[derive(Debug, Default)]
pub struct ReplaceFileContentTool;

#[derive(Debug, Deserialize)]
struct ReplacementChunk {
    /// The exact unique block of code to find.
    target_content: String,
    /// The replacement code.
    replacement_content: String,
}

#[derive(Debug, Deserialize)]
struct ReplaceArgs {
    path: String,
    edits: Vec<ReplacementChunk>,
}

#[async_trait]
impl Tool for ReplaceFileContentTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "replace_file_content".to_string(),
            description: "Surgically edit a file by finding and replacing unique, exact blocks of text. \
                          Always verify that the target_content is unique and matches the file content exactly, \
                          including leading whitespace and indentation. Avoids full-file rewrites."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "relative path to file" },
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "target_content": { "type": "string", "description": "exact unique block to find" },
                                "replacement_content": { "type": "string", "description": "replacement text" }
                            },
                            "required": ["target_content", "replacement_content"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: ReplaceArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = ctx.resolve(&a.path)?;

        let mut content = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolOutput::err(
                    format!("edit failed: file not found at {}", a.path),
                    json!({ "error": "file_not_found" }),
                ));
            }
            Err(e) => return Err(ToolError::Io(e)),
        };

        let mut lines_added = 0;
        let mut lines_removed = 0;

        for (idx, chunk) in a.edits.iter().enumerate() {
            let count = content.matches(&chunk.target_content).count();
            if count == 0 {
                return Ok(ToolOutput::err(
                    format!(
                        "edit failed: could not find exact block for chunk {} in {}",
                        idx + 1,
                        a.path
                    ),
                    json!({
                        "error": "target_not_found",
                        "chunk_index": idx + 1,
                        "target_content": chunk.target_content
                    }),
                ));
            }
            if count > 1 {
                return Ok(ToolOutput::err(
                    format!(
                        "edit failed: target content in chunk {} is not unique in {} (found {} matches)",
                        idx + 1,
                        a.path,
                        count
                    ),
                    json!({
                        "error": "target_not_unique",
                        "chunk_index": idx + 1,
                        "matches_count": count
                    }),
                ));
            }

            lines_removed += chunk.target_content.lines().count();
            lines_added += chunk.replacement_content.lines().count();
            content = content.replace(&chunk.target_content, &chunk.replacement_content);
        }

        tokio::fs::write(&path, &content).await?;

        Ok(ToolOutput::ok(
            format!(
                "surgically edited {} (applied {} chunk(s), +{} -{} lines)",
                a.path,
                a.edits.len(),
                lines_added,
                lines_removed
            ),
            json!({
                "path": a.path,
                "chunks_applied": a.edits.len(),
                "lines_added": lines_added,
                "lines_removed": lines_removed
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_surgical_replace_single() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();

        let tool = ReplaceFileContentTool;
        let out = tool
            .invoke(
                json!({
                    "path": "main.rs",
                    "edits": [
                        {
                            "target_content": "    println!(\"hello\");",
                            "replacement_content": "    println!(\"bonjour\");\n    let x = 42;"
                        }
                    ]
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!out.is_error);
        let updated = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(
            updated,
            "fn main() {\n    println!(\"bonjour\");\n    let x = 42;\n}\n"
        );
    }

    #[tokio::test]
    async fn test_surgical_replace_multi() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, "fn foo() {}\nfn bar() {}\nfn baz() {}\n").unwrap();

        let tool = ReplaceFileContentTool;
        let out = tool
            .invoke(
                json!({
                    "path": "main.rs",
                    "edits": [
                        {
                            "target_content": "fn foo() {}",
                            "replacement_content": "fn foo_optimized() {}"
                        },
                        {
                            "target_content": "fn baz() {}",
                            "replacement_content": "fn baz_optimized() {}"
                        }
                    ]
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!out.is_error);
        let updated = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(
            updated,
            "fn foo_optimized() {}\nfn bar() {}\nfn baz_optimized() {}\n"
        );
    }

    #[tokio::test]
    async fn test_surgical_replace_not_found() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, "fn main() {}\n").unwrap();

        let tool = ReplaceFileContentTool;
        let out = tool
            .invoke(
                json!({
                    "path": "main.rs",
                    "edits": [
                        {
                            "target_content": "fn missing() {}",
                            "replacement_content": "fn found() {}"
                        }
                    ]
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert_eq!(out.data["error"], "target_not_found");
    }

    #[tokio::test]
    async fn test_surgical_replace_not_unique() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, "let x = 1;\nlet x = 1;\n").unwrap();

        let tool = ReplaceFileContentTool;
        let out = tool
            .invoke(
                json!({
                    "path": "main.rs",
                    "edits": [
                        {
                            "target_content": "let x = 1;",
                            "replacement_content": "let x = 2;"
                        }
                    ]
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert_eq!(out.data["error"], "target_not_unique");
    }
}
