//! Documentation Web Fetcher Tool.
//!
//! Exposes a high-performance web documentation reader that fetches url
//! contents, strips script/style blocks, and renders clean Markdown.

use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::time::Duration;

#[derive(Debug, Default)]
pub struct FetchUrlTool;

#[derive(Debug, Deserialize)]
struct FetchUrlArgs {
    url: String,
}

#[async_trait]
impl Tool for FetchUrlTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fetch_url".to_string(),
            description: "Fetch web page content, strip HTML scaffolding, and convert it to clean readable markdown. Essential for reading library documentation."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "the absolute HTTP/HTTPS URL to retrieve" }
                },
                "required": ["url"]
            }),
            side_effects: true,
        }
    }

    async fn invoke(&self, args: Json, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: FetchUrlArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ToolError::Other(e.to_string()))?;

        let res = client
            .get(&a.url)
            .header("User-Agent", "JarvisAgent/0.1.0")
            .send()
            .await
            .map_err(|e| ToolError::Other(format!("http request failed: {e}")))?;

        let html = res
            .text()
            .await
            .map_err(|e| ToolError::Other(format!("failed to read response text: {e}")))?;

        let md = html_to_markdown(&html);

        Ok(ToolOutput::ok(
            format!(
                "fetched URL {} successfully ({} characters parsed)",
                a.url,
                md.len()
            ),
            json!({
                "url": a.url,
                "markdown": jarvis_core::clip(&md, 30_000)
            }),
        ))
    }
}

/// A lightweight, robust HTML-to-Markdown text formatter.
/// Strips out head, script, style, and svg blocks completely.
/// Formats h1-h6, lists, bold/italic elements, and paragraphs.
fn html_to_markdown(html: &str) -> String {
    // 1. Remove comments
    let mut clean_html = String::new();
    let mut in_comment = false;
    let chars: Vec<char> = html.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !in_comment && i + 4 <= chars.len() && chars[i..i + 4] == ['<', '!', '-', '-'] {
            in_comment = true;
            i += 4;
            continue;
        }
        if in_comment && i + 3 <= chars.len() && chars[i..i + 3] == ['-', '-', '>'] {
            in_comment = false;
            i += 3;
            continue;
        }
        if !in_comment {
            clean_html.push(chars[i]);
        }
        i += 1;
    }

    // 2. Remove script / style / head / svg tags and their content
    let mut stripped_html = String::new();
    let lower_html = clean_html.to_lowercase();
    let clean_chars: Vec<char> = clean_html.chars().collect();
    let lower_chars: Vec<char> = lower_html.chars().collect();
    let mut i = 0;

    let skip_tags = ["script", "style", "head", "svg", "noscript", "iframe"];

    while i < clean_chars.len() {
        let mut skipped = false;
        for tag in &skip_tags {
            let start_tag = format!("<{}", tag);
            let end_tag = format!("</{}", tag);

            if i + start_tag.len() <= clean_chars.len() {
                let segment: String = lower_chars[i..i + start_tag.len()].iter().collect();
                if segment == start_tag {
                    // Find closing tag
                    let mut found_end = false;
                    let mut j = i + start_tag.len();
                    while j + end_tag.len() <= clean_chars.len() {
                        let end_segment: String =
                            lower_chars[j..j + end_tag.len()].iter().collect();
                        if end_segment == end_tag {
                            j += end_tag.len();
                            while j < clean_chars.len() && clean_chars[j] != '>' {
                                j += 1;
                            }
                            if j < clean_chars.len() {
                                j += 1;
                            }
                            i = j;
                            found_end = true;
                            break;
                        }
                        j += 1;
                    }
                    if found_end {
                        skipped = true;
                        break;
                    }
                }
            }
        }
        if skipped {
            continue;
        }
        stripped_html.push(clean_chars[i]);
        i += 1;
    }

    // 3. Text formatting state machine
    let mut output = String::new();
    let chars: Vec<char> = stripped_html.chars().collect();
    let mut i = 0;

    let mut tag_stack: Vec<String> = Vec::new();
    let mut in_tag = false;
    let mut current_tag = String::new();
    let mut text_buf = String::new();

    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            if !text_buf.is_empty() {
                let trimmed = text_buf.trim();
                if !trimmed.is_empty() {
                    let mut formatted = trimmed.to_string();
                    if let Some(top) = tag_stack.last() {
                        if top == "strong" || top == "b" {
                            formatted = format!("**{}**", formatted);
                        } else if top == "em" || top == "i" {
                            formatted = format!("*{}*", formatted);
                        } else if top == "code" {
                            formatted = format!("`{}`", formatted);
                        }
                    }
                    output.push_str(&formatted);
                    output.push(' ');
                }
                text_buf.clear();
            }
            in_tag = true;
            current_tag.clear();
        } else if c == '>' {
            in_tag = false;
            let tag_content = current_tag.trim();
            if let Some(rest) = tag_content.strip_prefix('/') {
                let name = rest.trim().to_lowercase();
                if let Some(pos) = tag_stack.iter().rposition(|x| x == &name) {
                    tag_stack.remove(pos);
                }

                if matches!(
                    name.as_str(),
                    "p" | "div" | "section" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li"
                ) {
                    output.push('\n');
                }
            } else {
                let parts: Vec<&str> = tag_content.split_whitespace().collect();
                if !parts.is_empty() {
                    let name = parts[0].to_lowercase();
                    let is_self_closing = tag_content.ends_with('/');

                    if !is_self_closing {
                        tag_stack.push(name.clone());
                    }

                    if name == "h1" {
                        output.push_str("\n# ");
                    } else if name == "h2" {
                        output.push_str("\n## ");
                    } else if name == "h3" {
                        output.push_str("\n### ");
                    } else if name == "h4" {
                        output.push_str("\n#### ");
                    } else if name == "h5" {
                        output.push_str("\n##### ");
                    } else if name == "h6" {
                        output.push_str("\n###### ");
                    } else if name == "p" || name == "div" {
                        output.push('\n');
                    } else if name == "li" {
                        output.push_str("\n* ");
                    } else if name == "br" {
                        output.push('\n');
                    }
                }
            }
            current_tag.clear();
        } else if in_tag {
            current_tag.push(c);
        } else {
            text_buf.push(c);
        }
        i += 1;
    }

    if !text_buf.is_empty() {
        let trimmed = text_buf.trim();
        if !trimmed.is_empty() {
            output.push_str(trimmed);
        }
    }

    let mut final_output = String::new();
    let mut newline_count = 0;
    for line in output.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            newline_count += 1;
            if newline_count <= 2 {
                final_output.push('\n');
            }
        } else {
            newline_count = 0;
            final_output.push_str(trimmed_line);
            final_output.push('\n');
        }
    }

    final_output.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_to_markdown_formatting() {
        let html = r#"
            <html>
                <head>
                    <title>Test Page</title>
                    <style>body { color: red; }</style>
                    <script>console.log("hello");</script>
                </head>
                <body>
                    <h1>Main Header</h1>
                    <p>This is a <strong>bold</strong> paragraph.</p>
                    <ul>
                        <li>First item</li>
                        <li>Second item</li>
                    </ul>
                </body>
            </html>
        "#;

        let md = html_to_markdown(html);
        assert!(md.contains("# Main Header"));
        assert!(md.contains("This is a **bold** paragraph."));
        assert!(md.contains("* First item"));
        assert!(md.contains("* Second item"));

        // CSS and Script tags must be stripped
        assert!(!md.contains("color: red"));
        assert!(!md.contains("console.log"));
    }
}
