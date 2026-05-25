//! Stub LLM provider — emits a single canned `done` reply.
//!
//! Useful for testing the harness end-to-end without spinning up a real
//! model. The reply matches the agent's JSON contract so `parse_reply`
//! immediately classifies it as `ActionKind::Done` and the loop terminates
//! after exactly one step.

use async_trait::async_trait;
use futures::{Stream, stream};
use jarvis_core::{
    Capabilities, ChatRequest, ChatResponse, CompletionChunk, Error, LlmProvider, ProviderName,
    Result, Usage,
};
use std::pin::Pin;

const STUB_REPLY: &str =
    r#"{"action":"done","message":"stub provider — no LLM was actually called"}"#;

pub struct StubProvider {
    name: ProviderName,
    caps: Capabilities,
}

impl StubProvider {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: ProviderName::new(name.into()),
            caps: Capabilities {
                ctx_len: 32_000,
                tool_calls: true,
                json_schema: true,
                vision: false,
                supports_streaming: true,
            },
        }
    }
}

impl Default for StubProvider {
    fn default() -> Self {
        Self::new("local:stub")
    }
}

#[async_trait]
impl LlmProvider for StubProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    async fn complete(&self, _req: ChatRequest) -> Result<ChatResponse> {
        Ok(ChatResponse {
            content: STUB_REPLY.to_string(),
            usage: Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            },
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn complete_stream(
        &self,
        _req: ChatRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionChunk>> + Send + 'static>>> {
        // Emit one delta carrying the full reply, then a tail chunk carrying
        // usage (mirrors the OpenAI-compat streaming contract the agent loop
        // expects). Using `stream::iter` keeps things synchronous-ish — no
        // sleeps, no spawns, just immediate completion.
        let chunks: Vec<std::result::Result<CompletionChunk, Error>> = vec![
            Ok(CompletionChunk {
                delta: STUB_REPLY.to_string(),
                usage: None,
                finish_reason: None,
            }),
            Ok(CompletionChunk {
                delta: String::new(),
                usage: Some(Usage::default()),
                finish_reason: Some("stop".to_string()),
            }),
        ];
        Ok(Box::pin(stream::iter(chunks)))
    }
}

// `CompletionStream` is `Pin<Box<dyn Stream<Item = Result<CompletionChunk>> + Send + 'static>>`
// so we expose the path through `jarvis_core::provider::CompletionStream`.

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use jarvis_core::ChatMessage;

    #[tokio::test]
    async fn stub_complete_returns_done() {
        let p = StubProvider::default();
        let resp = p
            .complete(ChatRequest {
                messages: vec![ChatMessage::user("hi")],
                temperature: None,
                top_p: None,
                max_tokens: None,
                stream: false,
            })
            .await
            .unwrap();
        assert!(resp.content.contains("\"action\":\"done\""));
    }

    #[tokio::test]
    async fn stub_stream_emits_full_reply_and_usage() {
        let p = StubProvider::default();
        let mut s = p
            .complete_stream(ChatRequest {
                messages: vec![ChatMessage::user("hi")],
                temperature: None,
                top_p: None,
                max_tokens: None,
                stream: true,
            })
            .await
            .unwrap();
        let mut collected = String::new();
        let mut saw_usage = false;
        while let Some(c) = s.next().await {
            let c = c.unwrap();
            collected.push_str(&c.delta);
            if c.usage.is_some() {
                saw_usage = true;
            }
        }
        assert!(collected.contains("\"action\":\"done\""));
        assert!(saw_usage);
    }
}
