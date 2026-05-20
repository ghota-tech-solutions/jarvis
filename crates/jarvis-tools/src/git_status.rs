use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::process::Command;

#[derive(Debug, Default)]
pub struct GitStatusTool;

#[derive(Debug, Deserialize)]
struct GitStatusArgs {
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for GitStatusTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "git_status".to_string(),
            description: "Native cross-platform Git repository status reader. Returns branch name, staged changes, unstaged modified files, and untracked files as structured JSON, avoiding fragile and expensive shell executions.".to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "optional relative path; defaults to workdir" }
                }
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: GitStatusArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let root = match &a.path {
            Some(p) => ctx.resolve(p)?,
            None => ctx.workdir.clone(),
        };

        // Check if git is installed and it's a repository
        let output = match Command::new("git")
            .args(&["status", "--porcelain", "-b"])
            .current_dir(&root)
            .output()
            .await
        {
            Ok(out) => out,
            Err(e) => {
                return Ok(ToolOutput::ok(
                    "git command not found or failed to execute",
                    json!({
                        "is_git_repo": false,
                        "error": e.to_string()
                    })
                ));
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Ok(ToolOutput::ok(
                "not a git repository or git error",
                json!({
                    "is_git_repo": false,
                    "error": stderr.trim()
                })
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut branch = String::new();
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut untracked = Vec::new();

        for line in stdout.lines() {
            if line.is_empty() {
                continue;
            }
            if line.starts_with("##") {
                // Header line: branch information
                // E.g., "## main...origin/main" or "## HEAD (no branch)"
                let b = line[2..].trim();
                if let Some(pos) = b.find("...") {
                    branch = b[..pos].to_string();
                } else {
                    branch = b.to_string();
                }
            } else if line.len() >= 3 {
                let x = line.chars().nth(0).unwrap_or(' ');
                let y = line.chars().nth(1).unwrap_or(' ');
                let file = line[3..].trim().to_string();

                match (x, y) {
                    ('?', '?') => {
                        untracked.push(file);
                    }
                    (x, ' ') if x != ' ' => {
                        staged.push(file);
                    }
                    (' ', y) if y != ' ' => {
                        unstaged.push(file);
                    }
                    (x, y) if x != ' ' && y != ' ' => {
                        // Both staged and unstaged edits exist for this file
                        staged.push(file.clone());
                        unstaged.push(file);
                    }
                    _ => {}
                }
            }
        }

        Ok(ToolOutput::ok(
            format!("git status on branch '{}' (staged: {}, unstaged: {}, untracked: {})", branch, staged.len(), unstaged.len(), untracked.len()),
            json!({
                "is_git_repo": true,
                "branch": branch,
                "staged": staged,
                "unstaged": unstaged,
                "untracked": untracked
            })
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_git_status_non_repo() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolCtx::new(temp.path());
        let tool = GitStatusTool;
        let res = tool.invoke(json!({}), &ctx).await.unwrap();
        assert!(!res.is_error);
        let data = res.data;
        assert_eq!(data["is_git_repo"].as_bool(), Some(false));
    }
}
