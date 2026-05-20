//! Batch Multi-File Reader Tool.
//!
//! Exposes a parallel multi-file reader that allows agents to read
//! multiple files in a single turn, dramatically reducing round-trip latency.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value as Json, json};

#[derive(Debug, Default)]
pub struct FsReadManyTool;

#[derive(Debug, Deserialize)]
struct ReadManyArgs {
    paths: Vec<String>,
}

#[async_trait]
impl Tool for FsReadManyTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs_read_many".to_string(),
            description: "Read multiple UTF-8 text files simultaneously in parallel. Keeps token usage efficient and reduces agent round-trip turns."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "list of relative file paths"
                    }
                },
                "required": ["paths"]
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: ReadManyArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

        let mut results = Vec::with_capacity(a.paths.len());

        for rel_path in &a.paths {
            let path = match ctx.resolve(rel_path) {
                Ok(p) => p,
                Err(e) => {
                    results.push(json!({
                        "path": rel_path,
                        "success": false,
                        "error": format!("path escape error: {e}")
                    }));
                    continue;
                }
            };

            match tokio::fs::read_to_string(&path).await {
                Ok(content) => {
                    let total_lines = content.lines().count();
                    results.push(json!({
                        "path": rel_path,
                        "success": true,
                        "content": content,
                        "total_lines": total_lines
                    }));
                }
                Err(e) => {
                    results.push(json!({
                        "path": rel_path,
                        "success": false,
                        "error": e.to_string()
                    }));
                }
            }
        }

        let successful = results
            .iter()
            .filter(|r| r["success"].as_bool().unwrap_or(false))
            .count();

        Ok(ToolOutput::ok(
            format!(
                "read {}/{} files successfully",
                successful,
                a.paths.len()
            ),
            json!({ "results": results }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_read_many_success_and_failure() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());

        std::fs::write(dir.path().join("a.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hello").unwrap();

        let tool = FsReadManyTool;
        let out = tool
            .invoke(
                json!({
                    "paths": ["a.rs", "b.txt", "missing.rs", "../escape.rs"]
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!out.is_error);
        let results = out.data["results"].as_array().unwrap();
        assert_eq!(results.len(), 4);

        // a.rs
        assert_eq!(results[0]["path"], "a.rs");
        assert_eq!(results[0]["success"], true);
        assert_eq!(results[0]["content"], "fn main() {}");

        // b.txt
        assert_eq!(results[1]["path"], "b.txt");
        assert_eq!(results[1]["success"], true);
        assert_eq!(results[1]["content"], "hello");

        // missing.rs
        assert_eq!(results[2]["path"], "missing.rs");
        assert_eq!(results[2]["success"], false);
        let err_msg = results[2]["error"].as_str().unwrap().to_lowercase();
        assert!(err_msg.contains("notfound") || err_msg.contains("no such file") || err_msg.contains("directory"));

        // escape.rs
        assert_eq!(results[3]["path"], "../escape.rs");
        assert_eq!(results[3]["success"], false);
        assert!(results[3]["error"].as_str().unwrap().contains("escape"));
    }
}
