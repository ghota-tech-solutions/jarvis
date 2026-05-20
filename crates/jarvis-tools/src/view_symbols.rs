use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::path::Path;

#[derive(Debug, Default)]
pub struct ViewSymbolsTool;

#[derive(Debug, Deserialize)]
struct ViewSymbolsArgs {
    path: String,
}

#[derive(Debug, Serialize, Clone)]
struct SymbolInfo {
    name: String,
    line: usize,
    kind: String,
    signature: String,
}

#[async_trait]
impl Tool for ViewSymbolsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "view_symbols".to_string(),
            description: "Extract the structural outline of a file (functions, structs, impl blocks, classes, interfaces) without reading its full contents. Supports Rust, TypeScript/JavaScript, Python, and Go, saving enormous token context.".to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "relative path to target source file" }
                },
                "required": ["path"]
            }),
            side_effects: false,
        }
    }

    async fn invoke(&self, args: Json, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let a: ViewSymbolsArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = ctx.resolve(&a.path)?;

        let content = tokio::fs::read_to_string(&path).await
            .map_err(|e| ToolError::Other(format!("failed to read file: {e}")))?;

        let ext = Path::new(&a.path)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        let symbols = match ext.as_str() {
            "rs" => parse_rust(&content),
            "ts" | "tsx" | "js" | "jsx" => parse_js_ts(&content),
            "py" => parse_python(&content),
            "go" => parse_go(&content),
            _ => {
                return Ok(ToolOutput::err(
                    format!("unsupported file extension for symbol viewing: '.{}'", ext),
                    json!({ "supported_extensions": ["rs", "ts", "tsx", "js", "jsx", "py", "go"] })
                ));
            }
        };

        Ok(ToolOutput::ok(
            format!("extracted {} structural symbols from {}", symbols.len(), a.path),
            json!({
                "path": a.path,
                "symbols": symbols
            })
        ))
    }
}

fn parse_rust(content: &str) -> Vec<SymbolInfo> {
    let re_fn = Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    let re_struct = Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    let re_enum = Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    let re_trait = Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?trait\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    let re_impl = Regex::new(r"^\s*impl\b(.*)").unwrap();

    let mut symbols = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_num = idx + 1;
        let line_trimmed = line.trim();

        if let Some(caps) = re_fn.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "function".to_string(),
                signature: line_trimmed.to_string(),
            });
        } else if let Some(caps) = re_struct.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "struct".to_string(),
                signature: line_trimmed.to_string(),
            });
        } else if let Some(caps) = re_enum.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "enum".to_string(),
                signature: line_trimmed.to_string(),
            });
        } else if let Some(caps) = re_trait.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "trait".to_string(),
                signature: line_trimmed.to_string(),
            });
        } else if let Some(caps) = re_impl.captures(line) {
            let body = caps.get(1).map_or("", |m| m.as_str()).trim();
            let name = if body.contains(" for ") {
                body.split(" for ").nth(0).unwrap_or(body).trim().to_string()
            } else {
                body.split('{').nth(0).unwrap_or(body).trim().to_string()
            };
            symbols.push(SymbolInfo {
                name,
                line: line_num,
                kind: "impl".to_string(),
                signature: line_trimmed.to_string(),
            });
        }
    }
    symbols
}

fn parse_js_ts(content: &str) -> Vec<SymbolInfo> {
    let re_fn = Regex::new(r"^\s*(?:export\s+)?(?:async\s+)?function\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    let re_class = Regex::new(r"^\s*(?:export\s+)?class\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    let re_interface = Regex::new(r"^\s*(?:export\s+)?interface\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    let re_type = Regex::new(r"^\s*(?:export\s+)?type\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*=").unwrap();
    let re_arrow = Regex::new(r"^\s*(?:export\s+)?const\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*=\s*(?:async\s*)?\(.*?\)\s*=>").unwrap();

    let mut symbols = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_num = idx + 1;
        let line_trimmed = line.trim();

        if let Some(caps) = re_fn.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "function".to_string(),
                signature: line_trimmed.to_string(),
            });
        } else if let Some(caps) = re_class.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "class".to_string(),
                signature: line_trimmed.to_string(),
            });
        } else if let Some(caps) = re_interface.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "interface".to_string(),
                signature: line_trimmed.to_string(),
            });
        } else if let Some(caps) = re_type.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "type".to_string(),
                signature: line_trimmed.to_string(),
            });
        } else if let Some(caps) = re_arrow.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "arrow_function".to_string(),
                signature: line_trimmed.to_string(),
            });
        }
    }
    symbols
}

fn parse_python(content: &str) -> Vec<SymbolInfo> {
    let re_fn = Regex::new(r"^\s*def\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    let re_class = Regex::new(r"^\s*class\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();

    let mut symbols = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_num = idx + 1;
        let line_trimmed = line.trim();

        if let Some(caps) = re_fn.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "def".to_string(),
                signature: line_trimmed.to_string(),
            });
        } else if let Some(caps) = re_class.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "class".to_string(),
                signature: line_trimmed.to_string(),
            });
        }
    }
    symbols
}

fn parse_go(content: &str) -> Vec<SymbolInfo> {
    let re_fn = Regex::new(r"^\s*func\s+(?:\([^)]*\)\s+)?([a-zA-Z_][a-zA-Z0-9_]*)\s*\(").unwrap();
    let re_type = Regex::new(r"^\s*type\s+([a-zA-Z_][a-zA-Z0-9_]*)\s+(?:struct|interface)\b").unwrap();

    let mut symbols = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_num = idx + 1;
        let line_trimmed = line.trim();

        if let Some(caps) = re_fn.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "func".to_string(),
                signature: line_trimmed.to_string(),
            });
        } else if let Some(caps) = re_type.captures(line) {
            symbols.push(SymbolInfo {
                name: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                line: line_num,
                kind: "type".to_string(),
                signature: line_trimmed.to_string(),
            });
        }
    }
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_view_symbols_rust() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("main.rs");
        let code = r#"
            pub struct Config {
                pub port: u16,
            }

            impl Config {
                pub fn new() -> Self {
                    Config { port: 8080 }
                }
            }

            async fn run_server() {}
        "#;
        tokio::fs::write(&file_path, code).await.unwrap();

        let ctx = ToolCtx::new(temp.path());
        let tool = ViewSymbolsTool;
        let res = tool.invoke(json!({ "path": "main.rs" }), &ctx).await.unwrap();
        assert!(!res.is_error);

        let syms = res.data["symbols"].as_array().unwrap();
        assert_eq!(syms.len(), 4);
        assert_eq!(syms[0]["name"], "Config");
        assert_eq!(syms[0]["kind"], "struct");
        assert_eq!(syms[1]["name"], "Config");
        assert_eq!(syms[1]["kind"], "impl");
        assert_eq!(syms[2]["name"], "new");
        assert_eq!(syms[2]["kind"], "function");
        assert_eq!(syms[3]["name"], "run_server");
        assert_eq!(syms[3]["kind"], "function");
    }

    #[tokio::test]
    async fn test_view_symbols_ts() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("helper.ts");
        let code = r#"
            export interface User {
                id: string;
            }
            export class UserService {
                async getUser() {}
            }
            export const greet = (name: string) => {
                return `Hello ${name}`;
            }
        "#;
        tokio::fs::write(&file_path, code).await.unwrap();

        let ctx = ToolCtx::new(temp.path());
        let tool = ViewSymbolsTool;
        let res = tool.invoke(json!({ "path": "helper.ts" }), &ctx).await.unwrap();
        assert!(!res.is_error);

        let syms = res.data["symbols"].as_array().unwrap();
        assert_eq!(syms.len(), 3);
        assert_eq!(syms[0]["name"], "User");
        assert_eq!(syms[0]["kind"], "interface");
        assert_eq!(syms[1]["name"], "UserService");
        assert_eq!(syms[1]["kind"], "class");
        assert_eq!(syms[2]["name"], "greet");
        assert_eq!(syms[2]["kind"], "arrow_function");
    }
}
