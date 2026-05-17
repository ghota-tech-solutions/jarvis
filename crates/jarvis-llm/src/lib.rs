//! jarvis-llm — LLM provider implementations.
//!
//! M1 ships only the OpenAI-compatible provider, pointed at the local Gemma server.
//! M5 adds the DeepSeek remote provider, the model registry, and the capability-aware router.

mod openai_compat;

pub use openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
