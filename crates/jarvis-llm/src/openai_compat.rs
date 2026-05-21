//! OpenAI-compatible chat-completion provider.
//!
//! Works against any server that speaks the OpenAI `/v1/chat/completions` shape:
//! MLX-LM, llama.cpp, vLLM, OpenAI itself, DeepSeek, etc.

use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
        CreateChatCompletionRequestArgs,
    },
};
use async_trait::async_trait;
use futures_util::StreamExt;
use jarvis_core::{
    Capabilities, ChatMessage, ChatRequest, ChatResponse, ChatRole, CompletionChunk, Error,
    LlmProvider, ProviderName, Result, Usage,
};
use std::pin::Pin;
use tokio_stream::Stream;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct OpenAiCompatConfig {
    pub name: ProviderName,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub capabilities: Capabilities,
}

pub struct OpenAiCompatProvider {
    cfg: OpenAiCompatConfig,
    client: Client<OpenAIConfig>,
}

impl OpenAiCompatProvider {
    pub fn new(cfg: OpenAiCompatConfig) -> Self {
        let oai_cfg = OpenAIConfig::new()
            .with_api_base(cfg.base_url.clone())
            .with_api_key(cfg.api_key.clone());
        let client = Client::with_config(oai_cfg);
        Self { cfg, client }
    }

    fn convert_messages(msgs: &[ChatMessage]) -> Result<Vec<ChatCompletionRequestMessage>> {
        msgs.iter()
            .map(|m| -> Result<ChatCompletionRequestMessage> {
                match m.role {
                    ChatRole::System => Ok(ChatCompletionRequestSystemMessageArgs::default()
                        .content(m.content.clone())
                        .build()
                        .map_err(|e| Error::Provider(format!("system msg: {e}")))?
                        .into()),
                    ChatRole::User => Ok(ChatCompletionRequestUserMessageArgs::default()
                        .content(m.content.clone())
                        .build()
                        .map_err(|e| Error::Provider(format!("user msg: {e}")))?
                        .into()),
                    ChatRole::Assistant => Ok(ChatCompletionRequestAssistantMessageArgs::default()
                        .content(m.content.clone())
                        .build()
                        .map_err(|e| Error::Provider(format!("assistant msg: {e}")))?
                        .into()),
                    ChatRole::Tool => Err(Error::Invalid(
                        "tool messages not supported in M1".to_string(),
                    )),
                }
            })
            .collect()
    }
}

fn is_rate_limit_error(err: &async_openai::error::OpenAIError) -> bool {
    let err_str = err.to_string().to_lowercase();
    if err_str.contains("429")
        || err_str.contains("too many requests")
        || err_str.contains("rate limit")
    {
        return true;
    }
    if let async_openai::error::OpenAIError::Reqwest(req_err) = err
        && let Some(status) = req_err.status()
        && status.as_u16() == 429
    {
        return true;
    }
    false
}

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
    fn name(&self) -> &ProviderName {
        &self.cfg.name
    }

    fn capabilities(&self) -> Capabilities {
        self.cfg.capabilities
    }

    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse> {
        let mut attempt = 0;
        let max_attempts = 3;
        let mut delay = std::time::Duration::from_millis(500);

        loop {
            let mut builder = CreateChatCompletionRequestArgs::default();
            builder
                .model(&self.cfg.model)
                .messages(Self::convert_messages(&req.messages)?)
                .stream(false);
            if let Some(t) = req.temperature {
                builder.temperature(t);
            }
            if let Some(p) = req.top_p {
                builder.top_p(p);
            }
            if let Some(m) = req.max_tokens {
                builder.max_tokens(m);
            }
            let oai_req = builder
                .build()
                .map_err(|e| Error::Provider(format!("build request: {e}")))?;

            debug!(model = %self.cfg.model, "openai-compat: non-streaming complete");
            match self.client.chat().create(oai_req).await {
                Ok(resp) => {
                    let choice = resp
                        .choices
                        .into_iter()
                        .next()
                        .ok_or_else(|| Error::Provider("empty choices".to_string()))?;
                    let content = choice.message.content.unwrap_or_default();
                    let usage = resp
                        .usage
                        .map(|u| Usage {
                            prompt_tokens: u.prompt_tokens,
                            completion_tokens: u.completion_tokens,
                            total_tokens: u.total_tokens,
                        })
                        .unwrap_or_default();
                    return Ok(ChatResponse {
                        content,
                        usage,
                        finish_reason: choice
                            .finish_reason
                            .map(|fr| format!("{fr:?}").to_lowercase()),
                    });
                }
                Err(e) => {
                    if is_rate_limit_error(&e) && attempt < max_attempts {
                        attempt += 1;
                        warn!(
                            model = %self.cfg.model,
                            attempt = attempt,
                            delay_ms = delay.as_millis(),
                            "rate limited (429), retrying with backoff"
                        );
                        tokio::time::sleep(delay).await;
                        delay *= 2;
                    } else {
                        return Err(Error::Provider(format!("upstream: {e}")));
                    }
                }
            }
        }
    }

    async fn complete_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionChunk>> + Send + 'static>>> {
        let mut attempt = 0;
        let max_attempts = 3;
        let mut delay = std::time::Duration::from_millis(500);

        loop {
            let mut builder = CreateChatCompletionRequestArgs::default();
            builder
                .model(&self.cfg.model)
                .messages(Self::convert_messages(&req.messages)?)
                .stream(true);
            if let Some(t) = req.temperature {
                builder.temperature(t);
            }
            if let Some(p) = req.top_p {
                builder.top_p(p);
            }
            if let Some(m) = req.max_tokens {
                builder.max_tokens(m);
            }
            let oai_req = builder
                .build()
                .map_err(|e| Error::Provider(format!("build request: {e}")))?;

            debug!(model = %self.cfg.model, "openai-compat: streaming complete");
            match self.client.chat().create_stream(oai_req).await {
                Ok(raw) => {
                    let mapped = raw.map(|item| match item {
                        Ok(chunk) => {
                            let choice = chunk.choices.into_iter().next();
                            let delta = choice
                                .as_ref()
                                .and_then(|c| c.delta.content.clone())
                                .unwrap_or_default();
                            let finish_reason = choice
                                .and_then(|c| c.finish_reason)
                                .map(|fr| format!("{fr:?}").to_lowercase());
                            let usage = chunk.usage.map(|u| Usage {
                                prompt_tokens: u.prompt_tokens,
                                completion_tokens: u.completion_tokens,
                                total_tokens: u.total_tokens,
                            });
                            Ok(CompletionChunk {
                                delta,
                                usage,
                                finish_reason,
                            })
                        }
                        Err(e) => {
                            warn!(error = %e, "stream error");
                            Err(Error::Provider(format!("stream: {e}")))
                        }
                    });
                    return Ok(Box::pin(mapped));
                }
                Err(e) => {
                    if is_rate_limit_error(&e) && attempt < max_attempts {
                        attempt += 1;
                        warn!(
                            model = %self.cfg.model,
                            attempt = attempt,
                            delay_ms = delay.as_millis(),
                            "rate limited (429) on stream handshake, retrying with backoff"
                        );
                        tokio::time::sleep(delay).await;
                        delay *= 2;
                    } else {
                        return Err(Error::Provider(format!("upstream: {e}")));
                    }
                }
            }
        }
    }
}
