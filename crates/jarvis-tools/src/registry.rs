use crate::tool::ToolError;
use crate::{Tool, ToolCtx, ToolOutput};
use jsonschema::Validator;
use serde_json::Value as Json;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Pre-compiled JSON-Schema validators for each tool's `args_schema`.
    /// `None` for tools whose schema is empty / trivial (no validation needed),
    /// or whose schema failed to compile (logged once at registration).
    validators: HashMap<String, Arc<Option<Validator>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) -> &mut Self {
        let schema = tool.schema();
        let name = schema.name.clone();
        let validator = build_validator(&name, &schema.args_schema);
        self.validators.insert(name.clone(), Arc::new(validator));
        self.tools.insert(name, Arc::new(tool));
        self
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// § T1.3 — register the `search_tools` meta-tool. Call this *last*, after
    /// every other tool is registered, so `search_tools` sees the complete
    /// catalog. Idempotent: calling twice replaces the previous registration
    /// with a snapshot of the current registry.
    pub fn register_search_tools(&mut self) -> &mut Self {
        let catalog = self.schemas();
        let tool = crate::SearchToolsTool::new(catalog);
        self.register(tool)
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<_> = self.tools.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn schemas(&self) -> Vec<crate::ToolSchema> {
        let mut v: Vec<_> = self.tools.values().map(|t| t.schema()).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub async fn invoke(
        &self,
        name: &str,
        args: Json,
        ctx: &ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::InvalidArgs(format!("unknown tool `{name}`")))?;

        // Validate args against the tool's compiled JSON schema (if any).
        if let Some(validator_slot) = self.validators.get(name)
            && let Some(validator) = validator_slot.as_ref()
            && let Some(msg) = format_validation_errors(validator, &args)
        {
            return Err(ToolError::InvalidArgs(msg));
        }

        tool.invoke(args, ctx).await
    }
}

/// Decide whether a tool's `args_schema` is worth compiling. Returns `None`
/// (skip validation) for the trivial cases the task spec calls out:
/// `Value::Null`, `{}`, or `{"type":"object"}` with no `properties` /
/// `required` / `patternProperties` / `additionalProperties` constraints.
fn build_validator(tool_name: &str, schema: &Json) -> Option<Validator> {
    if is_trivial_schema(schema) {
        return None;
    }
    match jsonschema::validator_for(schema) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                tool = tool_name,
                error = %e,
                "tool args_schema failed to compile; arg validation will be skipped for this tool"
            );
            None
        }
    }
}

fn is_trivial_schema(schema: &Json) -> bool {
    match schema {
        Json::Null => true,
        Json::Object(map) => {
            if map.is_empty() {
                return true;
            }
            // Only trivial if it's just `{"type": "object"}` with nothing
            // else meaningful. Anything that declares properties / required
            // fields / additionalProperties is worth validating.
            map.len() == 1 && map.get("type").and_then(|v| v.as_str()) == Some("object")
        }
        _ => false,
    }
}

/// Run the validator and, if there are errors, format up to the first 5 of
/// them into a single line suitable for surfacing back to the LLM via
/// `ToolError::InvalidArgs`.
fn format_validation_errors(validator: &Validator, args: &Json) -> Option<String> {
    let mut errs = validator.iter_errors(args);
    let first = errs.next()?;
    let mut parts: Vec<String> = Vec::with_capacity(5);
    parts.push(format_single_error(&first));
    for e in errs.take(4) {
        parts.push(format_single_error(&e));
    }
    Some(format!("args validation failed: {}", parts.join("; ")))
}

fn format_single_error(e: &jsonschema::ValidationError<'_>) -> String {
    let path = e.instance_path().to_string();
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path
    };
    format!("{path}: {}", e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Minimal tool used to assert that validation happens BEFORE `invoke`.
    /// `invoke` flips `called` to true; tests verify it stays false on
    /// invalid args.
    struct FakeTool {
        called: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "fake".to_string(),
                description: "test tool".to_string(),
                args_schema: json!({
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": { "type": "string" }
                    }
                }),
                side_effects: false,
            }
        }

        async fn invoke(&self, _args: Json, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
            self.called.store(true, Ordering::SeqCst);
            Ok(ToolOutput::ok("done", json!({})))
        }
    }

    fn registry_with_fake() -> (ToolRegistry, Arc<AtomicBool>) {
        let called = Arc::new(AtomicBool::new(false));
        let mut reg = ToolRegistry::new();
        reg.register(FakeTool {
            called: called.clone(),
        });
        (reg, called)
    }

    #[tokio::test]
    async fn test_invoke_rejects_missing_required_field() {
        let (reg, called) = registry_with_fake();
        let ctx = ToolCtx::new(std::env::temp_dir());
        let res = reg.invoke("fake", json!({}), &ctx).await;
        let err = res.expect_err("missing required field should fail");
        match err {
            ToolError::InvalidArgs(msg) => {
                assert!(
                    msg.contains("path"),
                    "error should mention the missing field, got: {msg}"
                );
                assert!(
                    msg.contains("args validation failed"),
                    "error should be prefixed, got: {msg}"
                );
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
        assert!(
            !called.load(Ordering::SeqCst),
            "invoke must not be called when args are invalid"
        );
    }

    #[tokio::test]
    async fn test_invoke_rejects_wrong_type() {
        let (reg, called) = registry_with_fake();
        let ctx = ToolCtx::new(std::env::temp_dir());
        let res = reg.invoke("fake", json!({ "path": 42 }), &ctx).await;
        let err = res.expect_err("wrong type should fail");
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "expected InvalidArgs, got {err:?}"
        );
        assert!(
            !called.load(Ordering::SeqCst),
            "invoke must not be called when args are invalid"
        );
    }

    #[tokio::test]
    async fn test_invoke_accepts_valid_args() {
        let (reg, called) = registry_with_fake();
        let ctx = ToolCtx::new(std::env::temp_dir());
        let res = reg
            .invoke("fake", json!({ "path": "src/main.rs" }), &ctx)
            .await;
        assert!(res.is_ok(), "valid args should pass validation: {res:?}");
        assert!(
            called.load(Ordering::SeqCst),
            "invoke should have been reached"
        );
    }

    /// A tool with a trivial (empty) schema should not have any validation
    /// applied — any args object should reach `invoke`.
    #[tokio::test]
    async fn test_invoke_skips_validation_for_trivial_schema() {
        struct TrivialTool {
            called: Arc<AtomicBool>,
        }
        #[async_trait]
        impl Tool for TrivialTool {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "trivial".to_string(),
                    description: "no schema".to_string(),
                    args_schema: json!({ "type": "object" }),
                    side_effects: false,
                }
            }
            async fn invoke(&self, _args: Json, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
                self.called.store(true, Ordering::SeqCst);
                Ok(ToolOutput::ok("ok", json!({})))
            }
        }

        let called = Arc::new(AtomicBool::new(false));
        let mut reg = ToolRegistry::new();
        reg.register(TrivialTool {
            called: called.clone(),
        });
        let ctx = ToolCtx::new(std::env::temp_dir());
        // Pass garbage; the trivial schema must not reject it.
        let res = reg
            .invoke("trivial", json!({ "anything": [1, 2, 3] }), &ctx)
            .await;
        assert!(res.is_ok(), "trivial schema must skip validation: {res:?}");
        assert!(called.load(Ordering::SeqCst));
    }

    /// Smoke-test that every built-in tool's `args_schema` either compiles
    /// cleanly into a validator or is detected as trivial. This guards
    /// against a schema regression silently disabling validation for a
    /// production tool.
    #[test]
    fn test_builtin_tool_schemas_compile() {
        use crate::tool::Tool as _;
        use crate::{
            ApplyPatchTool, FetchUrlTool, FsReadManyTool, FsReadTool, FsWriteTool, GitStatusTool,
            GlobTool, GrepTool, ListDirTool, ReplaceFileContentTool, ShellTool, ViewSymbolsTool,
            WebSearchTool,
        };

        let schemas: Vec<crate::ToolSchema> = vec![
            FsReadTool.schema(),
            FsWriteTool.schema(),
            ShellTool.schema(),
            ApplyPatchTool.schema(),
            GrepTool.schema(),
            GlobTool.schema(),
            ReplaceFileContentTool.schema(),
            FsReadManyTool.schema(),
            ListDirTool.schema(),
            FetchUrlTool.schema(),
            GitStatusTool.schema(),
            ViewSymbolsTool.schema(),
            WebSearchTool.schema(),
        ];

        for s in schemas {
            if is_trivial_schema(&s.args_schema) {
                continue;
            }
            jsonschema::validator_for(&s.args_schema)
                .unwrap_or_else(|e| panic!("tool `{}` has an invalid args_schema: {e}", s.name));
        }
    }
}
