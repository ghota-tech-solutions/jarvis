use crate::{Capabilities, ProviderName, Result};
use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn system(s: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: s.into(),
        }
    }
    pub fn user(s: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: s.into(),
        }
    }
    pub fn assistant(s: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: s.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    /// Nucleus sampling cutoff. The Gemma 4 model card recommends 0.95.
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stream: bool,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stream: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub content: String,
    pub usage: Usage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionChunk {
    pub delta: String,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
}

pub type CompletionStream = Pin<Box<dyn Stream<Item = Result<CompletionChunk>> + Send + 'static>>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &ProviderName;
    fn capabilities(&self) -> Capabilities;

    /// Non-streaming convenience wrapper. Default impl collects the stream.
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse>;

    /// Streaming completion. Caller owns the stream and consumes it.
    async fn complete_stream(&self, req: ChatRequest) -> Result<CompletionStream>;
}
