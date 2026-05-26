//! § T2.9 v0 — browser sidecar tools.
//!
//! Three feature-gated tools that drive a headless Chrome/Chromium
//! over the CDP via `chromiumoxide`:
//!
//!   - `browser_navigate(url, [wait_until])` — open the URL, wait for
//!     the load event, return the final URL + title.
//!   - `browser_read([selector])` — return the textContent of the
//!     given CSS selector (default: `body`), trimmed and clipped to
//!     a sane size so a single page can't blow the context window.
//!   - `browser_screenshot([selector, fullscreen])` — return a base64
//!     PNG of the page (or of the matched element when a selector is
//!     given). Encoded inline rather than written to disk so the agent
//!     can paste it straight into a vision-capable provider.
//!
//! v0 spawns a fresh headless instance per invocation. Stateful
//! browsing (clicks, form fills, multi-step flows) is the v1
//! roadmap and will require a session manager + cookie persistence.
//!
//! Build with `--features browser`. Off by default to keep the
//! workspace build fast — Chromium / CDP types pull in a large
//! transitive graph and require an installed Chrome binary at
//! runtime. The launcher surfaces a clear hint when Chrome is
//! missing instead of failing opaquely.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::time::Duration;

const MAX_TEXT_CHARS: usize = 30_000;
const NAV_TIMEOUT: Duration = Duration::from_secs(20);

/// `browser_navigate` — open a URL and report what loaded.
#[derive(Debug, Default)]
pub struct BrowserNavigateTool;

#[derive(Debug, Deserialize)]
struct NavigateArgs {
    url: String,
}

#[async_trait]
impl Tool for BrowserNavigateTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_navigate".to_string(),
            description: "Open a URL in a headless Chrome and return the final URL + page title once loaded. Use as the first step before `browser_read` or `browser_screenshot`.".to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Absolute HTTP(S) URL to open" }
                },
                "required": ["url"]
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: NavigateArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let (mut browser, mut handler) = launch_browser().await?;
        let handler_task = tokio::spawn(async move {
            while let Some(_h) = handler.next().await { /* drain events */ }
        });

        let result = async {
            let page = tokio::time::timeout(NAV_TIMEOUT, browser.new_page(&a.url))
                .await
                .map_err(|_| {
                    ToolError::Other(format!("navigation timed out after {NAV_TIMEOUT:?}"))
                })?
                .map_err(|e| ToolError::Other(format!("open_page: {e}")))?;
            let _ = page.wait_for_navigation().await;
            let final_url = page
                .url()
                .await
                .map_err(|e| ToolError::Other(format!("page.url: {e}")))?
                .unwrap_or(a.url.clone());
            let title = page
                .get_title()
                .await
                .map_err(|e| ToolError::Other(format!("get_title: {e}")))?
                .unwrap_or_default();
            Ok::<_, ToolError>((final_url, title))
        }
        .await;

        let _ = browser.close().await;
        handler_task.abort();
        let (final_url, title) = result?;

        Ok(ToolOutput::ok(
            format!("loaded {final_url} ({title})"),
            json!({ "url": final_url, "title": title }),
        ))
    }
}

/// `browser_read` — extract trimmed text from a CSS selector.
#[derive(Debug, Default)]
pub struct BrowserReadTool;

#[derive(Debug, Deserialize)]
struct ReadArgs {
    url: String,
    #[serde(default = "default_selector")]
    selector: String,
}

fn default_selector() -> String {
    "body".to_string()
}

#[async_trait]
impl Tool for BrowserReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_read".to_string(),
            description: "Open a URL and return the visible textContent of a CSS selector (default `body`). Use when `fetch_url` returns garbage because the page is client-rendered.".to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Absolute HTTP(S) URL to load" },
                    "selector": {
                        "type": "string",
                        "description": "Optional CSS selector to extract. Defaults to `body`.",
                        "default": "body"
                    }
                },
                "required": ["url"]
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: ReadArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let (mut browser, mut handler) = launch_browser().await?;
        let handler_task =
            tokio::spawn(async move { while let Some(_h) = handler.next().await {} });

        let result = async {
            let page = tokio::time::timeout(NAV_TIMEOUT, browser.new_page(&a.url))
                .await
                .map_err(|_| {
                    ToolError::Other(format!("navigation timed out after {NAV_TIMEOUT:?}"))
                })?
                .map_err(|e| ToolError::Other(format!("open_page: {e}")))?;
            let _ = page.wait_for_navigation().await;
            let el = page.find_element(&a.selector).await.map_err(|e| {
                ToolError::Other(format!("selector `{}` not found: {e}", a.selector))
            })?;
            let raw = el
                .inner_text()
                .await
                .map_err(|e| ToolError::Other(format!("inner_text: {e}")))?
                .unwrap_or_default();
            Ok::<_, ToolError>(raw)
        }
        .await;

        let _ = browser.close().await;
        handler_task.abort();
        let raw = result?;

        let text = jarvis_core::clip(raw.trim(), MAX_TEXT_CHARS);
        Ok(ToolOutput::ok(
            format!(
                "read {} characters from `{}` at {}",
                text.len(),
                a.selector,
                a.url
            ),
            json!({ "url": a.url, "selector": a.selector, "text": text }),
        ))
    }
}

/// `browser_screenshot` — capture a base64 PNG of the page (or element).
#[derive(Debug, Default)]
pub struct BrowserScreenshotTool;

#[derive(Debug, Deserialize)]
struct ScreenshotArgs {
    url: String,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    fullscreen: bool,
}

#[async_trait]
impl Tool for BrowserScreenshotTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_screenshot".to_string(),
            description: "Capture a screenshot of the page (or a specific element) as base64-encoded PNG. Useful for visual debugging or paste-as-image into a vision-capable LLM turn.".to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Absolute HTTP(S) URL to load" },
                    "selector": { "type": "string", "description": "Optional CSS selector. If set, only that element is captured." },
                    "fullscreen": { "type": "boolean", "description": "Whether to capture the full scrollable page (`true`) or just the viewport (`false`, default). Ignored when `selector` is set." }
                },
                "required": ["url"]
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: ScreenshotArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let (mut browser, mut handler) = launch_browser().await?;
        let handler_task =
            tokio::spawn(async move { while let Some(_h) = handler.next().await {} });

        let result = async {
            let page = tokio::time::timeout(NAV_TIMEOUT, browser.new_page(&a.url))
                .await
                .map_err(|_| {
                    ToolError::Other(format!("navigation timed out after {NAV_TIMEOUT:?}"))
                })?
                .map_err(|e| ToolError::Other(format!("open_page: {e}")))?;
            let _ = page.wait_for_navigation().await;
            let bytes = if let Some(sel) = &a.selector {
                let el = page
                    .find_element(sel)
                    .await
                    .map_err(|e| ToolError::Other(format!("selector `{sel}` not found: {e}")))?;
                el.screenshot(CaptureScreenshotFormat::Png)
                    .await
                    .map_err(|e| ToolError::Other(format!("element.screenshot: {e}")))?
            } else {
                let params = ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .full_page(a.fullscreen)
                    .build();
                page.screenshot(params)
                    .await
                    .map_err(|e| ToolError::Other(format!("page.screenshot: {e}")))?
            };
            Ok::<_, ToolError>(bytes)
        }
        .await;

        let _ = browser.close().await;
        handler_task.abort();
        let bytes = result?;
        let b64 = B64.encode(&bytes);

        Ok(ToolOutput::ok(
            format!("captured {} bytes png", bytes.len()),
            json!({
                "url": a.url,
                "selector": a.selector,
                "fullscreen": a.fullscreen,
                "png_base64": b64,
                "size_bytes": bytes.len(),
            }),
        ))
    }
}

/// Launch a headless Chrome with sane defaults. The handler stream
/// MUST be polled by the caller (otherwise CDP responses queue
/// forever and every page op deadlocks).
async fn launch_browser() -> Result<(Browser, chromiumoxide::handler::Handler), ToolError> {
    let config = BrowserConfig::builder()
        .build()
        .map_err(|e| ToolError::Other(format!("browser config: {e}")))?;
    Browser::launch(config).await.map_err(|e| {
        ToolError::Other(format!(
            "failed to launch headless Chrome: {e} — install Chrome/Chromium and ensure it is on PATH",
        ))
    })
}
