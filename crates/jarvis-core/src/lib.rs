//! jarvis-core — shared types, traits, errors. No I/O.

mod capabilities;
mod error;
mod ids;
mod provider;
mod routing;

pub use capabilities::{Capabilities, RequiredCapabilities};
pub use error::{Error, Result};
pub use ids::{AgentId, EventId, ProviderName, TaskId};
pub use provider::{
    ChatMessage, ChatRequest, ChatResponse, ChatRole, CompletionChunk, LlmProvider, Usage,
};
pub use routing::{RoutingPolicy, TaskKind};
