//! jarvis-llm — LLM provider implementations + model registry + capability-aware
//! pool with quarantine and failover.

mod openai_compat;
mod pool;
mod registry;

pub use openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
pub use pool::{LlmPool, ModelStatus, PickRequest, PickedModel, PoolError, QuarantineConfig};
pub use registry::{make_openai_compat_entry, ModelEntry, ModelKind, ModelRegistry};
