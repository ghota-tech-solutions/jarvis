use crate::ProviderName;
use serde::{Deserialize, Serialize};

/// User-facing routing policy. Set per-task or in config defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum RoutingPolicy {
    /// Automatic selection by the router.
    #[default]
    Auto,
    /// Only providers under `[providers.local.*]` are eligible.
    LocalOnly,
    /// Only providers under `[providers.remote.*]` are eligible.
    RemoteOnly,
    /// Force a specific provider; fail if unavailable.
    Model(ProviderName),
}

/// Categories used by the router to apply Auto rules.
/// M1 uses only `Simple` and `Planning`; expanded in M5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Simple,
    Planning,
    DeepRefactor,
    Architecture,
    Reviewer,
    SimpleEdit,
    Summarize,
    Classify,
    Format,
    JsonExtract,
    VisionQa,
}
