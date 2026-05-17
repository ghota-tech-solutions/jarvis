use crate::{Tool, ToolCtx, ToolOutput};
use crate::tool::ToolError;
use serde_json::Value as Json;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) -> &mut Self {
        let schema = tool.schema();
        self.tools.insert(schema.name, Arc::new(tool));
        self
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
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
        tool.invoke(args, ctx).await
    }
}
