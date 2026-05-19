//! Web search tool — generic over backend.
//!
//! Two backends ship out of the box: **Brave** and **Tavily**. Both are
//! free-tier-friendly and the agent only needs a search-snippets view, so
//! we normalise to `[{title, url, snippet}]` and cap at 10 results.
//!
//! Backend selection: env-driven so the user can set it once.
//!   - `JARVIS_SEARCH_BACKEND` = `brave` | `tavily` (default: brave if
//!     `BRAVE_API_KEY` is present, else tavily if `TAVILY_API_KEY` is
//!     present, else the tool fails with a helpful message).
//!   - `BRAVE_API_KEY` / `TAVILY_API_KEY`: API credentials.
//!
//! The tool is `side_effects: false` — it only reads from external APIs
//! and the LLM is allowed to call it from `read_only` sandbox mode.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value as Json, json};

const MAX_RESULTS: usize = 10;

pub struct WebSearchTool;

#[derive(Debug, Deserialize)]
struct Args {
    /// Search query.
    q: String,
    /// Optional results cap (1..=10).
    #[serde(default)]
    count: Option<u32>,
}

#[derive(Debug, Clone)]
struct Hit {
    title: String,
    url: String,
    snippet: String,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search".to_string(),
            description: "Web search — returns up to 10 results [{title, url, snippet}]. \
                Backend selected via JARVIS_SEARCH_BACKEND (brave|tavily); \
                falls back to whichever API key is available."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "q":     { "type": "string",  "description": "search query" },
                    "count": { "type": "integer", "description": "1..=10, default 5" }
                },
                "required": ["q"]
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: Args =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let q = a.q.trim();
        if q.is_empty() {
            return Err(ToolError::InvalidArgs("query is empty".into()));
        }
        let count = a.count.unwrap_or(5).clamp(1, MAX_RESULTS as u32) as usize;
        let backend = select_backend()?;
        let hits = backend.search(q, count).await.map_err(ToolError::Other)?;
        let summary = format!("{} result(s) for `{}`", hits.len(), q);
        let data = json!({
            "backend": backend.name(),
            "query": q,
            "results": hits
                .iter()
                .map(|h| json!({"title": h.title, "url": h.url, "snippet": h.snippet}))
                .collect::<Vec<_>>(),
        });
        Ok(ToolOutput::ok(summary, data))
    }
}

#[async_trait]
trait SearchBackend: Send + Sync {
    fn name(&self) -> &'static str;
    async fn search(&self, q: &str, count: usize) -> Result<Vec<Hit>, String>;
}

fn select_backend() -> Result<Box<dyn SearchBackend>, ToolError> {
    let chosen = std::env::var("JARVIS_SEARCH_BACKEND").ok();
    let has_brave = std::env::var("BRAVE_API_KEY").is_ok();
    let has_tavily = std::env::var("TAVILY_API_KEY").is_ok();
    match chosen.as_deref() {
        Some("brave") => Ok(Box::new(BraveBackend)),
        Some("tavily") => Ok(Box::new(TavilyBackend)),
        _ if has_brave => Ok(Box::new(BraveBackend)),
        _ if has_tavily => Ok(Box::new(TavilyBackend)),
        _ => Err(ToolError::Other(
            "no search backend configured: set BRAVE_API_KEY or TAVILY_API_KEY (and optionally JARVIS_SEARCH_BACKEND)".into(),
        )),
    }
}

// ---------- Brave backend ----------

struct BraveBackend;

#[async_trait]
impl SearchBackend for BraveBackend {
    fn name(&self) -> &'static str {
        "brave"
    }

    async fn search(&self, q: &str, count: usize) -> Result<Vec<Hit>, String> {
        let key = std::env::var("BRAVE_API_KEY").map_err(|_| "BRAVE_API_KEY unset".to_string())?;
        let url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            urlencoding(q),
            count,
        );
        let resp = reqwest::Client::new()
            .get(&url)
            .header("X-Subscription-Token", key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("brave http {status}: {}", clip(&body, 200)));
        }
        let v: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| format!("brave parse: {e}"))?;
        let results = v
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(results
            .into_iter()
            .take(count)
            .map(|r| Hit {
                title: r
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: r
                    .get("url")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                snippet: r
                    .get("description")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
            .collect())
    }
}

// ---------- Tavily backend ----------

struct TavilyBackend;

#[async_trait]
impl SearchBackend for TavilyBackend {
    fn name(&self) -> &'static str {
        "tavily"
    }

    async fn search(&self, q: &str, count: usize) -> Result<Vec<Hit>, String> {
        let key =
            std::env::var("TAVILY_API_KEY").map_err(|_| "TAVILY_API_KEY unset".to_string())?;
        let payload = json!({
            "api_key": key,
            "query": q,
            "max_results": count,
            "search_depth": "basic",
        });
        let resp = reqwest::Client::new()
            .post("https://api.tavily.com/search")
            .header("Content-Type", "application/json")
            .body(payload.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("tavily http {status}: {}", clip(&body, 200)));
        }
        let v: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| format!("tavily parse: {e}"))?;
        let results = v
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(results
            .into_iter()
            .take(count)
            .map(|r| Hit {
                title: r
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: r
                    .get("url")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                snippet: r
                    .get("content")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
            .collect())
    }
}

// ---------- helpers ----------

fn urlencoding(s: &str) -> String {
    // Minimal RFC-3986 encoding for the bits that show up in queries.
    // We don't pull a whole urlencode crate for one call.
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut s = s[..n].to_string();
        s.push('…');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding_basics() {
        assert_eq!(urlencoding("hello world"), "hello+world");
        assert_eq!(urlencoding("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencoding("foo-bar.baz~qux_1"), "foo-bar.baz~qux_1");
    }

    #[test]
    fn select_backend_errors_when_no_keys() {
        // SAFETY: tests are single-threaded by default; unset both keys and the env override.
        unsafe {
            std::env::remove_var("BRAVE_API_KEY");
            std::env::remove_var("TAVILY_API_KEY");
            std::env::remove_var("JARVIS_SEARCH_BACKEND");
        }
        match select_backend() {
            Ok(_) => panic!("expected error when no keys are set"),
            Err(e) => assert!(e.to_string().contains("no search backend configured")),
        }
    }
}
