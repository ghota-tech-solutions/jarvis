//! `grep` and `glob` tools — first-class code search so the agent doesn't
//! shell out to `Get-ChildItem -Recurse | Select-String` (Windows) or
//! `find … | xargs grep` (Unix). Both pipelines:
//!   - flood the model's context with file paths it doesn't care about,
//!   - hit the Windows console codepage mojibake we just patched in
//!     `jarvis-sandbox::native::shell_cmd`,
//!   - and need ad-hoc syntax knowledge for each platform.
//!
//! These tools use the `ignore` walker (the same one ripgrep uses) so
//! `.gitignore` / `.ignore` / hidden dirs are respected by default — the
//! agent never wastes a turn searching through `target/` or `node_modules/`.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::path::Path;

// ------------------ grep ------------------

#[derive(Debug, Default)]
pub struct GrepTool;

#[derive(Debug, Deserialize)]
struct GrepArgs {
    /// Regex pattern (Rust regex syntax — same as ripgrep).
    pattern: String,
    /// Optional subdir relative to workdir. Defaults to the whole workdir.
    #[serde(default)]
    path: Option<String>,
    /// Optional glob filter on file paths (e.g. "*.rs", "**/*.toml").
    #[serde(default)]
    glob: Option<String>,
    /// Case-insensitive match. Default false.
    #[serde(default)]
    ignore_case: bool,
    /// Cap on the number of matches returned. Default 100, hard max 500.
    #[serde(default)]
    max_results: Option<usize>,
}

#[async_trait]
impl Tool for GrepTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "grep".to_string(),
            description:
                "Search file contents for a regex pattern under the workdir. Returns each \
                 match as `path:line:text`. Respects `.gitignore` and skips hidden / binary \
                 files automatically. Use this instead of shelling out to `Select-String` / \
                 `grep` — it returns just the matches with no shell noise."
                    .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "pattern":     { "type": "string", "description": "Rust regex (same syntax as ripgrep)" },
                    "path":        { "type": "string", "description": "subdir relative to workdir; default whole workdir" },
                    "glob":        { "type": "string", "description": "glob filter on file paths, e.g. \"*.rs\", \"**/*.toml\"" },
                    "ignore_case": { "type": "boolean" },
                    "max_results": { "type": "integer", "description": "default 100, hard max 500" }
                },
                "required": ["pattern"]
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: GrepArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let root = match &a.path {
            Some(p) => ctx.resolve(p)?,
            None => ctx.workdir.clone(),
        };
        let pattern = a.pattern.clone();
        let glob = a.glob.clone();
        let case = a.ignore_case;
        let cap = a.max_results.unwrap_or(100).min(500);
        let workdir = ctx.workdir.clone();

        let res = tokio::task::spawn_blocking(move || {
            grep_blocking(&root, &workdir, &pattern, glob.as_deref(), case, cap)
        })
        .await
        .map_err(|e| ToolError::Other(format!("join: {e}")))?;

        match res {
            Ok((matches, total, truncated)) => {
                let summary = if total == 0 {
                    "0 matches".to_string()
                } else if truncated {
                    format!("{}+ matches (showing first {})", total, matches.len())
                } else {
                    format!("{total} matches")
                };
                Ok(ToolOutput::ok(
                    summary,
                    json!({
                        "matches": matches,
                        "total": total,
                        "truncated": truncated,
                    }),
                ))
            }
            Err(e) => Ok(ToolOutput::err(
                format!("grep error: {e}"),
                json!({ "error": e }),
            )),
        }
    }
}

fn grep_blocking(
    root: &Path,
    workdir: &Path,
    pattern: &str,
    glob: Option<&str>,
    ignore_case: bool,
    cap: usize,
) -> Result<(Vec<serde_json::Value>, usize, bool), String> {
    let re = RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|e| format!("invalid pattern: {e}"))?;
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false);
    if let Some(g) = glob {
        let mut ob = OverrideBuilder::new(root);
        ob.add(g).map_err(|e| format!("invalid glob: {e}"))?;
        let ov = ob.build().map_err(|e| format!("invalid glob: {e}"))?;
        builder.overrides(ov);
    }
    let walker = builder.build();
    let mut matches: Vec<serde_json::Value> = Vec::with_capacity(cap.min(64));
    let mut total = 0usize;

    for dent in walker.flatten() {
        let path = dent.path();
        if !path.is_file() {
            continue;
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        // Skip binary-looking files: any NUL byte in the first 8 KB.
        let head = &bytes[..bytes.len().min(8192)];
        if head.contains(&0) {
            continue;
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => continue, // skip non-UTF-8
        };
        for (lineno, line) in text.lines().enumerate() {
            if re.is_match(line) {
                total += 1;
                if matches.len() < cap {
                    let rel = path.strip_prefix(workdir).unwrap_or(path);
                    matches.push(json!({
                        "path": display_rel(rel),
                        "line": lineno + 1,
                        "text": jarvis_core::clip(line, 200),
                    }));
                }
            }
        }
        // Stop walking early if we've already counted way past the cap; the
        // agent gets a clear "truncated" signal and can narrow the pattern.
        if total >= cap * 5 {
            break;
        }
    }
    let truncated = total > matches.len();
    Ok((matches, total, truncated))
}

fn display_rel(p: &Path) -> String {
    // Normalize Windows backslashes to forward slashes so paths are stable
    // across platforms (and easier for the LLM to quote).
    p.to_string_lossy().replace('\\', "/")
}

// ------------------ glob ------------------

#[derive(Debug, Default)]
pub struct GlobTool;

#[derive(Debug, Deserialize)]
struct GlobArgs {
    /// Glob pattern (e.g. "**/*.rs", "src/**/mod.rs").
    pattern: String,
    /// Cap on the number of paths returned. Default 200, hard max 1000.
    #[serde(default)]
    max_results: Option<usize>,
}

#[async_trait]
impl Tool for GlobTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "glob".to_string(),
            description:
                "List paths under the workdir matching a glob pattern. Respects `.gitignore` \
                 and skips hidden dirs. Use this instead of shelling out to `Get-ChildItem` / \
                 `find` — paths come back already filtered."
                    .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "pattern":     { "type": "string", "description": "e.g. \"**/*.rs\", \"src/**/mod.rs\"" },
                    "max_results": { "type": "integer", "description": "default 200, hard max 1000" }
                },
                "required": ["pattern"]
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: GlobArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let pattern = a.pattern.clone();
        let cap = a.max_results.unwrap_or(200).min(1000);
        let workdir = ctx.workdir.clone();

        let res = tokio::task::spawn_blocking(move || glob_blocking(&workdir, &pattern, cap))
            .await
            .map_err(|e| ToolError::Other(format!("join: {e}")))?;
        match res {
            Ok((paths, total, truncated)) => {
                let summary = if total == 0 {
                    "0 paths".to_string()
                } else if truncated {
                    format!("{}+ paths (showing first {})", total, paths.len())
                } else {
                    format!("{total} paths")
                };
                Ok(ToolOutput::ok(
                    summary,
                    json!({
                        "paths": paths,
                        "total": total,
                        "truncated": truncated,
                    }),
                ))
            }
            Err(e) => Ok(ToolOutput::err(
                format!("glob error: {e}"),
                json!({ "error": e }),
            )),
        }
    }
}

fn glob_blocking(
    workdir: &Path,
    pattern: &str,
    cap: usize,
) -> Result<(Vec<String>, usize, bool), String> {
    let mut ob = OverrideBuilder::new(workdir);
    ob.add(pattern).map_err(|e| format!("invalid glob: {e}"))?;
    let ov = ob.build().map_err(|e| format!("invalid glob: {e}"))?;
    let mut builder = WalkBuilder::new(workdir);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .overrides(ov);
    let walker = builder.build();
    let mut paths: Vec<String> = Vec::with_capacity(cap.min(64));
    let mut total = 0usize;
    for dent in walker.flatten() {
        // Skip the workdir itself and directories.
        if dent.path() == workdir {
            continue;
        }
        if dent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        total += 1;
        if paths.len() < cap {
            let rel = dent.path().strip_prefix(workdir).unwrap_or(dent.path());
            paths.push(display_rel(rel));
        }
    }
    let truncated = total > paths.len();
    Ok((paths, total, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn grep_finds_pattern_with_glob_filter() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\nfn helper() {}\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "fn this_is_text\n").unwrap();
        let out = GrepTool
            .invoke(json!({ "pattern": "fn ", "glob": "*.rs" }), &ctx)
            .await
            .unwrap();
        let matches = out.data["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert!(
            matches
                .iter()
                .all(|m| m["path"].as_str().unwrap().ends_with("a.rs"))
        );
    }

    #[tokio::test]
    async fn grep_respects_case_insensitive() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        std::fs::write(dir.path().join("a.txt"), "Hello\nHELLO\nworld\n").unwrap();
        let out = GrepTool
            .invoke(json!({ "pattern": "hello", "ignore_case": true }), &ctx)
            .await
            .unwrap();
        assert_eq!(out.data["total"].as_u64().unwrap(), 2);
    }

    #[tokio::test]
    async fn glob_returns_matching_paths() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        std::fs::create_dir_all(dir.path().join("src/sub")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/sub/mod.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        let out = GlobTool
            .invoke(json!({ "pattern": "**/*.rs" }), &ctx)
            .await
            .unwrap();
        let paths: Vec<&str> = out.data["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("src/lib.rs")));
        assert!(paths.iter().any(|p| p.ends_with("src/sub/mod.rs")));
        assert!(!paths.iter().any(|p| p.ends_with("README.md")));
    }

    #[tokio::test]
    async fn grep_skips_binary_files() {
        let dir = tempdir().unwrap();
        let ctx = ToolCtx::new(dir.path());
        std::fs::write(dir.path().join("text.txt"), "needle\n").unwrap();
        std::fs::write(
            dir.path().join("blob.bin"),
            [b'a', 0, b'n', b'e', b'e', b'd', b'l', b'e'],
        )
        .unwrap();
        let out = GrepTool
            .invoke(json!({ "pattern": "needle" }), &ctx)
            .await
            .unwrap();
        let matches = out.data["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0]["path"].as_str().unwrap().ends_with("text.txt"));
    }
}
