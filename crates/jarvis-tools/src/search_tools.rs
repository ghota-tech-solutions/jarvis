//! § T1.3 — `search_tools` meta-tool.
//!
//! When the registry hosts many tools (12 builtins + N MCP-imported), dumping
//! every JSON schema into the system prompt costs ~3–5 k tokens per turn.
//! With lazy schemas enabled the prompt advertises only names + one-line
//! descriptions, and the model calls `search_tools(query)` on demand to
//! retrieve full schemas. This module ships the meta-tool itself; the
//! eager-vs-lazy rendering decision lives in `jarvis-agent::prompt`.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use serde_json::{Value as Json, json};

/// Meta-tool that returns the schemas of tools matching a free-form query.
/// Built from a frozen catalog at construction time so it cannot drift from
/// the registry — register it *last* via
/// [`crate::ToolRegistry::register_search_tools`].
pub struct SearchToolsTool {
    catalog: Vec<ToolSchema>,
}

impl SearchToolsTool {
    pub fn new(catalog: Vec<ToolSchema>) -> Self {
        Self { catalog }
    }

    /// Canonical name — kept as a public const so call-sites (registry, prompt
    /// renderer) don't drift from the schema definition.
    pub const NAME: &'static str = "search_tools";
}

#[async_trait]
impl Tool for SearchToolsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.to_string(),
            description: "Find tools by keyword and return their full JSON-schema args. Useful when the prompt only shows tool names — call this to learn how to invoke a specific tool. Returns the best-matching schemas, ranked by relevance.".to_string(),
            args_schema: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search terms (matched against tool name and description, case-insensitive)."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 20,
                        "description": "Max number of results to return. Default 5."
                    }
                }
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_default();
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(5)
            .clamp(1, 20);

        // Empty query → no useful ranking; return a hint rather than the
        // whole catalog.
        if query.is_empty() {
            return Ok(ToolOutput::ok(
                "empty query — pass a search term (tool name or keyword)",
                json!({ "query": "", "matches": [] }),
            ));
        }

        let query_words: Vec<&str> = query.split_whitespace().filter(|w| !w.is_empty()).collect();

        let mut scored: Vec<(i32, &ToolSchema)> = self
            .catalog
            .iter()
            .filter(|s| s.name != Self::NAME)
            .map(|s| {
                let name_lower = s.name.to_lowercase();
                let desc_lower = s.description.to_lowercase();
                let mut score = 0i32;
                // Exact name match dominates.
                if name_lower == query {
                    score += 100;
                }
                // Substring of name = strong signal.
                if name_lower.contains(&query) {
                    score += 10;
                }
                // Per-word matches add up; name hits weigh more than
                // description hits.
                for word in &query_words {
                    if name_lower.contains(word) {
                        score += 5;
                    }
                    if desc_lower.contains(word) {
                        score += 1;
                    }
                }
                (score, s)
            })
            .filter(|(s, _)| *s > 0)
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        scored.truncate(limit);

        let matches: Vec<Json> = scored
            .iter()
            .map(|(_, s)| serde_json::to_value(s).unwrap_or(Json::Null))
            .collect();
        let count = matches.len();
        let summary = if count == 0 {
            format!("no tool matches `{query}`")
        } else if count == 1 {
            format!("1 match: {}", scored[0].1.name)
        } else {
            let preview: Vec<&str> = scored
                .iter()
                .take(3)
                .map(|(_, s)| s.name.as_str())
                .collect();
            format!("{count} matches: {}", preview.join(", "))
        };

        Ok(ToolOutput::ok(
            summary,
            json!({ "query": query, "matches": matches }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_catalog() -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: "fs_read".into(),
                description: "Read a UTF-8 file from the workdir.".into(),
                args_schema: json!({}),
                side_effects: false,
            },
            ToolSchema {
                name: "fs_write".into(),
                description: "Write a file inside the workdir.".into(),
                args_schema: json!({}),
                side_effects: true,
            },
            ToolSchema {
                name: "shell".into(),
                description: "Run a shell command via the active sandbox.".into(),
                args_schema: json!({}),
                side_effects: true,
            },
            ToolSchema {
                name: "grep".into(),
                description: "Regex search across files in the workdir.".into(),
                args_schema: json!({}),
                side_effects: false,
            },
        ]
    }

    fn ctx() -> ToolCtx {
        ToolCtx::new(PathBuf::from("/tmp"))
    }

    #[tokio::test]
    async fn exact_name_match_wins() {
        let t = SearchToolsTool::new(fake_catalog());
        let out = t
            .invoke(json!({ "query": "fs_read" }), &ctx())
            .await
            .unwrap();
        let matches = out.data["matches"].as_array().unwrap();
        assert!(!matches.is_empty());
        assert_eq!(matches[0]["name"], "fs_read");
    }

    #[tokio::test]
    async fn partial_name_match_returned() {
        let t = SearchToolsTool::new(fake_catalog());
        let out = t.invoke(json!({ "query": "fs" }), &ctx()).await.unwrap();
        let matches = out.data["matches"].as_array().unwrap();
        let names: Vec<&str> = matches
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"fs_read"));
        assert!(names.contains(&"fs_write"));
    }

    #[tokio::test]
    async fn description_match_works() {
        let t = SearchToolsTool::new(fake_catalog());
        let out = t.invoke(json!({ "query": "regex" }), &ctx()).await.unwrap();
        let matches = out.data["matches"].as_array().unwrap();
        assert_eq!(matches[0]["name"], "grep");
    }

    #[tokio::test]
    async fn empty_query_returns_hint() {
        let t = SearchToolsTool::new(fake_catalog());
        let out = t.invoke(json!({ "query": "" }), &ctx()).await.unwrap();
        assert!(out.summary.contains("empty query"));
        assert!(out.data["matches"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_match_returns_empty_matches() {
        let t = SearchToolsTool::new(fake_catalog());
        let out = t
            .invoke(json!({ "query": "xyzzyplugh" }), &ctx())
            .await
            .unwrap();
        assert!(out.summary.contains("no tool matches"));
        assert!(out.data["matches"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_tools_excludes_itself() {
        let mut cat = fake_catalog();
        cat.push(ToolSchema {
            name: "search_tools".into(),
            description: "self".into(),
            args_schema: json!({}),
            side_effects: false,
        });
        let t = SearchToolsTool::new(cat);
        let out = t
            .invoke(json!({ "query": "search" }), &ctx())
            .await
            .unwrap();
        let matches = out.data["matches"].as_array().unwrap();
        for m in matches {
            assert_ne!(m["name"].as_str(), Some("search_tools"));
        }
    }

    #[tokio::test]
    async fn limit_is_honored() {
        let t = SearchToolsTool::new(fake_catalog());
        // Every tool's description contains "the workdir" — broad query.
        let out = t
            .invoke(json!({ "query": "workdir", "limit": 2 }), &ctx())
            .await
            .unwrap();
        let matches = out.data["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
    }
}
