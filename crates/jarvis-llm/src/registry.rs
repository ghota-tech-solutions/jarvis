//! Model registry: the immutable catalogue of providers known to the daemon.
//!
//! The pool layers mutable quarantine state on top of this catalogue.

use crate::OpenAiCompatProvider;
use jarvis_core::{Capabilities, LlmProvider, ProviderName, ToolDialect};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Local,
    Remote,
}

impl ModelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }
}

#[derive(Clone)]
pub struct ModelEntry {
    pub name: ProviderName,
    pub kind: ModelKind,
    pub model_id: String,
    pub priority: i32,
    pub capabilities: Capabilities,
    pub cost_per_mtok_in: f64,
    pub cost_per_mtok_out: f64,
    pub provider: Arc<dyn LlmProvider>,
    /// § C.M-B — which tool-call contract the agent loop should use for
    /// this model. Defaults to `Json` (universal). Configure per-model in
    /// `jarvis.toml` via the `tool_dialect = "gemma4_strict"` field.
    pub tool_dialect: ToolDialect,
    /// Gemma 4 reflection / thinking mode. When true the agent prepends
    /// `<|think|>` to the system prompt. Off by default.
    pub thinking: bool,
}

impl std::fmt::Debug for ModelEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelEntry")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("model_id", &self.model_id)
            .field("priority", &self.priority)
            .field("capabilities", &self.capabilities)
            .field("cost_per_mtok_in", &self.cost_per_mtok_in)
            .field("cost_per_mtok_out", &self.cost_per_mtok_out)
            .field("tool_dialect", &self.tool_dialect)
            .field("thinking", &self.thinking)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default)]
pub struct ModelRegistry {
    by_name: HashMap<ProviderName, ModelEntry>,
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, entry: ModelEntry) {
        self.by_name.insert(entry.name.clone(), entry);
    }

    pub fn get(&self, name: &ProviderName) -> Option<&ModelEntry> {
        self.by_name.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ModelEntry> {
        self.by_name.values()
    }

    pub fn names(&self) -> Vec<ProviderName> {
        let mut v: Vec<_> = self.by_name.keys().cloned().collect();
        v.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        v
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }
}

/// Convenience: build an OpenAI-compatible provider into a model entry.
#[allow(clippy::too_many_arguments)]
pub fn make_openai_compat_entry(
    name: impl Into<String>,
    kind: ModelKind,
    base_url: impl Into<String>,
    model_id: impl Into<String>,
    api_key: impl Into<String>,
    priority: i32,
    capabilities: Capabilities,
    cost_per_mtok_in: f64,
    cost_per_mtok_out: f64,
) -> ModelEntry {
    let model_id = model_id.into();
    let qualified = ProviderName::new(format!("{}:{}", kind.as_str(), name.into()));
    let cfg = crate::OpenAiCompatConfig {
        name: qualified.clone(),
        base_url: base_url.into(),
        model: model_id.clone(),
        api_key: api_key.into(),
        capabilities,
    };
    let provider: Arc<dyn LlmProvider> = Arc::new(OpenAiCompatProvider::new(cfg));
    ModelEntry {
        name: qualified,
        kind,
        model_id,
        priority,
        capabilities,
        cost_per_mtok_in,
        cost_per_mtok_out,
        provider,
        tool_dialect: ToolDialect::default(),
        thinking: false,
    }
}
