use serde::{Deserialize, Serialize};

/// Capabilities advertised (or probed) for a single model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub ctx_len: u32,
    pub tool_calls: bool,
    pub json_schema: bool,
    pub vision: bool,
    pub supports_streaming: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            ctx_len: 8192,
            tool_calls: false,
            json_schema: false,
            vision: false,
            supports_streaming: true,
        }
    }
}

impl Capabilities {
    /// Returns true iff this capability set covers the requirement.
    pub fn satisfies(&self, req: &RequiredCapabilities) -> bool {
        req.min_ctx.is_none_or(|m| self.ctx_len >= m)
            && (!req.tool_calls || self.tool_calls)
            && (!req.json_schema || self.json_schema)
            && (!req.vision || self.vision)
            && (!req.streaming || self.supports_streaming)
    }
}

/// What an outgoing request requires from its model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredCapabilities {
    pub min_ctx: Option<u32>,
    pub tool_calls: bool,
    pub json_schema: bool,
    pub vision: bool,
    pub streaming: bool,
}
