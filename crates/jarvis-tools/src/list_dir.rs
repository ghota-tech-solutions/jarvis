//! Platform-Agnostic Directory Listing Tool.
//!
//! Exposes a structured directory listing tool using the `ignore` crate
//! to respect `.gitignore` rules, skip binary/hidden files, and return
//! structured, platform-independent JSON entries.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::path::Path;

#[derive(Debug, Default)]
pub struct ListDirTool;

#[derive(Debug, Deserialize)]
struct ListDirArgs {
    /// Optional relative path to list. Defaults to the workdir root.
    #[serde(default)]
    path: Option<String>,
    /// Optional depth limit. Defaults to 2.
    #[serde(default)]
    max_depth: Option<usize>,
}

#[async_trait]
impl Tool for ListDirTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_dir".to_string(),
            description: "List files and subdirectories. Respects .gitignore, skips hidden files, and displays file sizes. Defaults to a max depth of 2 to keep context cheap."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "optional relative path; defaults to workdir root" },
                    "max_depth": { "type": "integer", "description": "depth limit; defaults to 2" }
                }
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: ListDirArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let root = match &a.path {
            Some(p) => ctx.resolve(p)?,
            None => ctx.workdir.clone(),
        };

        let depth = a.max_depth.unwrap_or(2);
        let workdir = ctx.workdir.clone();

        let entries =
            tokio::task::spawn_blocking(move || list_dir_blocking(&root, &workdir, depth))
                .await
                .map_err(|e| ToolError::Other(format!("join: {e}")))?;

        Ok(ToolOutput::ok(
            format!(
                "listed {} entries under {}",
                entries.len(),
                a.path.unwrap_or_else(|| ".".into())
            ),
            json!({ "entries": entries }),
        ))
    }
}

fn list_dir_blocking(root: &Path, workdir: &Path, max_depth: usize) -> Vec<serde_json::Value> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .max_depth(Some(max_depth));

    let mut entries = Vec::new();
    for dent in builder.build().flatten() {
        let path = dent.path();
        if path == root {
            continue;
        }

        let is_dir = path.is_dir();
        let size = path.metadata().map(|m| m.len()).unwrap_or(0);
        let rel = path.strip_prefix(workdir).unwrap_or(path);

        entries.push(json!({
            "path": display_rel(rel),
            "is_dir": is_dir,
            "size_bytes": if is_dir { 0 } else { size }
        }));
    }

    // Sort entries for deterministic output
    entries.sort_by(|a, b| a["path"].as_str().unwrap().cmp(b["path"].as_str().unwrap()));
    entries
}

fn display_rel(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_list_dir_respects_gitignore() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());

        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn ok() {}").unwrap();
        std::fs::write(dir.path().join("target_file.txt"), "hello").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target_file.txt\n").unwrap();

        let tool = ListDirTool;
        let out = tool.invoke(json!({}), &ctx).await.unwrap();

        assert!(!out.is_error);
        let entries = out.data["entries"].as_array().unwrap();

        // Should include src/lib.rs, but skip target_file.txt (which is gitignored)
        let paths: Vec<&str> = entries
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();

        assert!(paths.contains(&"src"));
        assert!(paths.contains(&"src/lib.rs"));
        assert!(!paths.contains(&"target_file.txt"));
    }
}
