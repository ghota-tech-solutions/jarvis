//! Capability-aware LLM pool with quarantine and failover.
//!
//! The pool wraps a `ModelRegistry` and adds:
//!   * `probe_all()` — sends a 1-token request to each model at boot, narrows
//!     declared capabilities to what was actually observed.
//!   * `pick(req)` — filters by required capabilities, applies the routing
//!     policy, returns the highest-priority compatible model.
//!   * `record_success / record_failure` — moves quarantine state forward.
//!
//! Quarantine: per-model rolling window of failure timestamps. If more than
//! `cfg.threshold` failures fall inside `cfg.window`, the model is marked
//! `quarantined_until = now + cfg.duration` and skipped by `pick()`.

use crate::registry::{ModelEntry, ModelKind, ModelRegistry};
use chrono::{DateTime, Duration, Utc};
use jarvis_core::{
    ChatMessage, ChatRequest, ChatResponse, LlmProvider, ProviderName, RequiredCapabilities,
    RoutingPolicy, TaskKind,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("no model matches the request: {0}")]
    NoMatch(String),
    #[error("forced model `{0}` not found")]
    ForcedNotFound(ProviderName),
    #[error("forced model `{0}` is quarantined")]
    ForcedQuarantined(ProviderName),
    #[error("registry empty")]
    Empty,
}

#[derive(Debug, Clone, Copy)]
pub struct QuarantineConfig {
    pub threshold: u32,
    pub window: Duration,
    pub duration: Duration,
}

impl Default for QuarantineConfig {
    fn default() -> Self {
        Self {
            threshold: 3,
            window: Duration::minutes(10),
            duration: Duration::minutes(15),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct FailureWindow {
    times: VecDeque<DateTime<Utc>>,
    quarantined_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct ModelStatus {
    pub name: ProviderName,
    pub kind: ModelKind,
    pub online: bool,
    pub quarantined: bool,
    pub quarantined_until: Option<DateTime<Utc>>,
    pub failures_in_window: u32,
    pub priority: i32,
    pub model_id: String,
}

#[derive(Clone)]
pub struct PickRequest {
    pub required: RequiredCapabilities,
    pub kind: TaskKind,
    pub estimated_tokens: usize,
    pub routing_override: Option<RoutingPolicy>,
    pub forbidden: HashSet<ProviderName>,
}

impl std::fmt::Debug for PickRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickRequest")
            .field("required", &self.required)
            .field("kind", &self.kind)
            .field("estimated_tokens", &self.estimated_tokens)
            .field("routing_override", &self.routing_override)
            .field("forbidden_count", &self.forbidden.len())
            .finish()
    }
}

impl PickRequest {
    pub fn for_planning() -> Self {
        Self {
            required: RequiredCapabilities::default(),
            kind: TaskKind::Planning,
            estimated_tokens: 0,
            routing_override: None,
            forbidden: HashSet::new(),
        }
    }
}

#[derive(Clone)]
pub struct PickedModel {
    pub name: ProviderName,
    pub kind: ModelKind,
    pub provider: Arc<dyn LlmProvider>,
    pub model_id: String,
    /// § C.M-B — propagated from `ModelEntry::tool_dialect` so the agent
    /// loop knows which system prompt + response preprocessing to apply.
    pub tool_dialect: jarvis_core::ToolDialect,
}

impl std::fmt::Debug for PickedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickedModel")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("model_id", &self.model_id)
            .field("tool_dialect", &self.tool_dialect)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct LlmPool {
    registry: Arc<ModelRegistry>,
    state: Arc<RwLock<PoolState>>,
    cfg: QuarantineConfig,
}

#[derive(Default)]
struct PoolState {
    /// Live observed capabilities (≤ declared after probe).
    observed: HashMap<ProviderName, jarvis_core::Capabilities>,
    /// Per-model failure window.
    failures: HashMap<ProviderName, FailureWindow>,
    /// Has the model passed `probe_all()` ?
    online: HashMap<ProviderName, bool>,
}

impl LlmPool {
    pub fn new(registry: ModelRegistry, cfg: QuarantineConfig) -> Self {
        Self {
            registry: Arc::new(registry),
            state: Arc::new(RwLock::new(PoolState::default())),
            cfg,
        }
    }

    pub fn registry(&self) -> &ModelRegistry {
        &self.registry
    }

    pub fn quarantine_config(&self) -> QuarantineConfig {
        self.cfg
    }

    /// Boot-time probe: hit each provider with a 1-token request, observe whether
    /// it responds, narrow capabilities if necessary.
    pub async fn probe_all(&self) {
        for entry in self.registry.iter() {
            let req = ChatRequest {
                messages: vec![
                    ChatMessage::system("Reply with exactly the single word: ok"),
                    ChatMessage::user("ping"),
                ],
                temperature: Some(0.0),
                max_tokens: Some(8),
                stream: false,
            };
            match entry.provider.complete(req).await {
                Ok(ChatResponse { content, .. }) => {
                    let ok = !content.trim().is_empty();
                    let mut s = self.state.write().await;
                    s.observed.insert(entry.name.clone(), entry.capabilities);
                    s.online.insert(entry.name.clone(), ok);
                    if ok {
                        info!(model = %entry.name, model_id = %entry.model_id, "probe ok");
                    } else {
                        warn!(model = %entry.name, "probe returned empty");
                    }
                }
                Err(e) => {
                    warn!(model = %entry.name, error = %e, "probe failed");
                    let mut s = self.state.write().await;
                    s.online.insert(entry.name.clone(), false);
                }
            }
        }
    }

    /// Record a successful call. Clears the failure window so a model recovers.
    pub async fn record_success(&self, name: &ProviderName) {
        let mut s = self.state.write().await;
        if let Some(w) = s.failures.get_mut(name) {
            w.times.clear();
            w.quarantined_until = None;
        }
        s.online.insert(name.clone(), true);
    }

    /// Record a failure. Returns true if the model is now quarantined.
    pub async fn record_failure(&self, name: &ProviderName) -> bool {
        let now = Utc::now();
        let cutoff = now - self.cfg.window;
        let mut s = self.state.write().await;
        let entry = s.failures.entry(name.clone()).or_default();
        entry.times.push_back(now);
        while let Some(t) = entry.times.front() {
            if *t < cutoff {
                entry.times.pop_front();
            } else {
                break;
            }
        }
        if entry.times.len() as u32 >= self.cfg.threshold {
            entry.quarantined_until = Some(now + self.cfg.duration);
            warn!(model = %name, until = ?entry.quarantined_until, "quarantined");
            true
        } else {
            false
        }
    }

    /// Snapshot of every model's status for the TUI / status RPC.
    pub async fn status_all(&self) -> Vec<ModelStatus> {
        let s = self.state.read().await;
        let now = Utc::now();
        let mut out = Vec::with_capacity(self.registry.len());
        for entry in self.registry.iter() {
            let fw = s.failures.get(&entry.name);
            let quarantined_until = fw.and_then(|w| w.quarantined_until).filter(|t| *t > now);
            out.push(ModelStatus {
                name: entry.name.clone(),
                kind: entry.kind,
                online: s.online.get(&entry.name).copied().unwrap_or(false),
                quarantined: quarantined_until.is_some(),
                quarantined_until,
                failures_in_window: fw.map(|w| w.times.len() as u32).unwrap_or(0),
                priority: entry.priority,
                model_id: entry.model_id.clone(),
            });
        }
        out.sort_by(|a, b| {
            (
                a.kind as u8,
                std::cmp::Reverse(a.priority),
                a.name.as_str().to_string(),
            )
                .cmp(&(
                    b.kind as u8,
                    std::cmp::Reverse(b.priority),
                    b.name.as_str().to_string(),
                ))
        });
        out
    }

    /// Pick a model for a request, applying the filter → policy → tie-break chain.
    pub async fn pick(&self, req: &PickRequest) -> Result<PickedModel, PoolError> {
        if self.registry.is_empty() {
            return Err(PoolError::Empty);
        }
        let state = self.state.read().await;
        let now = Utc::now();
        let policy = req.routing_override.clone().unwrap_or_default();

        // Strict force first.
        if let RoutingPolicy::Model(name) = &policy {
            let entry = self
                .registry
                .get(name)
                .ok_or_else(|| PoolError::ForcedNotFound(name.clone()))?;
            if is_quarantined(state.failures.get(name), now) {
                return Err(PoolError::ForcedQuarantined(name.clone()));
            }
            return Ok(picked(entry));
        }

        let mut candidates: Vec<&ModelEntry> = self
            .registry
            .iter()
            .filter(|e| match &policy {
                RoutingPolicy::LocalOnly => e.kind == ModelKind::Local,
                RoutingPolicy::RemoteOnly => e.kind == ModelKind::Remote,
                _ => true,
            })
            .filter(|e| !req.forbidden.contains(&e.name))
            .filter(|e| !is_quarantined(state.failures.get(&e.name), now))
            // Models that failed boot probe stay out until a re-probe (or daemon
            // restart) flips them back. Unset (never probed) is treated as online.
            .filter(|e| state.online.get(&e.name).copied().unwrap_or(true))
            .filter(|e| {
                let caps = state
                    .observed
                    .get(&e.name)
                    .copied()
                    .unwrap_or(e.capabilities);
                caps.satisfies(&req.required)
            })
            .filter(|e| {
                if req.estimated_tokens == 0 {
                    true
                } else {
                    (req.estimated_tokens as u32) < (e.capabilities.ctx_len.saturating_mul(7)) / 10
                }
            })
            .collect();

        if candidates.is_empty() {
            return Err(PoolError::NoMatch(format!(
                "policy={:?} required={:?} kind={:?} forbidden={}",
                policy,
                req.required,
                req.kind,
                req.forbidden.len()
            )));
        }

        // Apply Auto rules: prefer Remote for heavy reasoning, Local for light edits.
        if matches!(policy, RoutingPolicy::Auto) {
            let prefer_remote = matches!(
                req.kind,
                TaskKind::Planning
                    | TaskKind::DeepRefactor
                    | TaskKind::Architecture
                    | TaskKind::Reviewer
            );
            let prefer_local = matches!(
                req.kind,
                TaskKind::SimpleEdit
                    | TaskKind::Summarize
                    | TaskKind::Classify
                    | TaskKind::Format
                    | TaskKind::JsonExtract
            );
            if prefer_remote && candidates.iter().any(|e| e.kind == ModelKind::Remote) {
                candidates.retain(|e| e.kind == ModelKind::Remote);
            } else if prefer_local && candidates.iter().any(|e| e.kind == ModelKind::Local) {
                candidates.retain(|e| e.kind == ModelKind::Local);
            }
        }

        // Tie-break: highest priority, then lowest input cost.
        candidates.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| {
                    a.cost_per_mtok_in
                        .partial_cmp(&b.cost_per_mtok_in)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.name.as_str().cmp(b.name.as_str()))
        });

        Ok(picked(candidates[0]))
    }
}

fn picked(entry: &ModelEntry) -> PickedModel {
    PickedModel {
        name: entry.name.clone(),
        kind: entry.kind,
        provider: entry.provider.clone(),
        model_id: entry.model_id.clone(),
        tool_dialect: entry.tool_dialect,
    }
}

fn is_quarantined(w: Option<&FailureWindow>, now: DateTime<Utc>) -> bool {
    w.and_then(|w| w.quarantined_until)
        .map(|t| t > now)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ModelKind, make_openai_compat_entry};
    use jarvis_core::Capabilities;

    fn registry_two_local() -> ModelRegistry {
        let mut r = ModelRegistry::new();
        r.insert(make_openai_compat_entry(
            "gemma",
            ModelKind::Local,
            "http://x/v1",
            "gemma-4-26B",
            "k",
            10,
            Capabilities {
                ctx_len: 128_000,
                tool_calls: true,
                json_schema: true,
                vision: false,
                supports_streaming: true,
            },
            0.0,
            0.0,
        ));
        r.insert(make_openai_compat_entry(
            "qwen",
            ModelKind::Local,
            "http://x/v1",
            "qwen3-32B",
            "k",
            8,
            Capabilities {
                ctx_len: 32_000,
                tool_calls: true,
                json_schema: true,
                vision: false,
                supports_streaming: true,
            },
            0.0,
            0.0,
        ));
        r
    }

    fn registry_with_remote() -> ModelRegistry {
        let mut r = registry_two_local();
        r.insert(make_openai_compat_entry(
            "deepseek_pro",
            ModelKind::Remote,
            "http://x/v1",
            "deepseek-v4-pro",
            "k",
            5,
            Capabilities {
                ctx_len: 200_000,
                tool_calls: true,
                json_schema: true,
                vision: false,
                supports_streaming: true,
            },
            0.27,
            1.10,
        ));
        r
    }

    fn registry_with_vision() -> ModelRegistry {
        let mut r = registry_two_local();
        r.insert(make_openai_compat_entry(
            "vision",
            ModelKind::Local,
            "http://x/v1",
            "qwen2-vl-7b",
            "k",
            3,
            Capabilities {
                ctx_len: 32_000,
                tool_calls: false,
                json_schema: false,
                vision: true,
                supports_streaming: true,
            },
            0.0,
            0.0,
        ));
        r
    }

    #[tokio::test]
    async fn auto_picks_highest_priority_local_for_simple_edit() {
        let pool = LlmPool::new(registry_two_local(), QuarantineConfig::default());
        let mut req = PickRequest::for_planning();
        req.kind = TaskKind::SimpleEdit;
        let picked = pool.pick(&req).await.unwrap();
        assert_eq!(picked.name.as_str(), "local:gemma");
    }

    #[tokio::test]
    async fn auto_prefers_remote_for_planning_when_available() {
        let pool = LlmPool::new(registry_with_remote(), QuarantineConfig::default());
        let req = PickRequest {
            kind: TaskKind::Planning,
            ..PickRequest::for_planning()
        };
        let picked = pool.pick(&req).await.unwrap();
        assert_eq!(picked.kind, ModelKind::Remote);
    }

    #[tokio::test]
    async fn vision_requirement_picks_vision_model() {
        let pool = LlmPool::new(registry_with_vision(), QuarantineConfig::default());
        let req = PickRequest {
            required: RequiredCapabilities {
                vision: true,
                ..Default::default()
            },
            kind: TaskKind::VisionQa,
            ..PickRequest::for_planning()
        };
        let picked = pool.pick(&req).await.unwrap();
        assert_eq!(picked.name.as_str(), "local:vision");
    }

    #[tokio::test]
    async fn quarantine_after_threshold_failures() {
        let pool = LlmPool::new(
            registry_two_local(),
            QuarantineConfig {
                threshold: 3,
                ..Default::default()
            },
        );
        let name = ProviderName::new("local:gemma");
        assert!(!pool.record_failure(&name).await);
        assert!(!pool.record_failure(&name).await);
        let quarantined = pool.record_failure(&name).await;
        assert!(quarantined);

        // Pick should skip the quarantined one.
        let req = PickRequest {
            kind: TaskKind::SimpleEdit,
            ..PickRequest::for_planning()
        };
        let picked = pool.pick(&req).await.unwrap();
        assert_eq!(picked.name.as_str(), "local:qwen");
    }

    #[tokio::test]
    async fn record_success_clears_window() {
        let pool = LlmPool::new(
            registry_two_local(),
            QuarantineConfig {
                threshold: 2,
                ..Default::default()
            },
        );
        let name = ProviderName::new("local:gemma");
        pool.record_failure(&name).await;
        pool.record_success(&name).await;
        // Now one more failure shouldn't yet quarantine.
        assert!(!pool.record_failure(&name).await);
    }

    #[tokio::test]
    async fn forced_model_not_found() {
        let pool = LlmPool::new(registry_two_local(), QuarantineConfig::default());
        let req = PickRequest {
            routing_override: Some(RoutingPolicy::Model(ProviderName::new("local:nope"))),
            ..PickRequest::for_planning()
        };
        assert!(matches!(
            pool.pick(&req).await,
            Err(PoolError::ForcedNotFound(_))
        ));
    }

    #[tokio::test]
    async fn local_only_policy_drops_remote() {
        let pool = LlmPool::new(registry_with_remote(), QuarantineConfig::default());
        let req = PickRequest {
            kind: TaskKind::Planning,
            routing_override: Some(RoutingPolicy::LocalOnly),
            ..PickRequest::for_planning()
        };
        let picked = pool.pick(&req).await.unwrap();
        assert_eq!(picked.kind, ModelKind::Local);
    }

    #[tokio::test]
    async fn offline_models_are_skipped() {
        let pool = LlmPool::new(registry_with_remote(), QuarantineConfig::default());
        // Mark deepseek offline manually.
        {
            let mut s = pool.state.write().await;
            s.online
                .insert(ProviderName::new("remote:deepseek_pro"), false);
        }
        let req = PickRequest {
            kind: TaskKind::Planning,
            ..PickRequest::for_planning()
        };
        // Auto would normally pick remote for planning; offline kicks it out.
        let picked = pool.pick(&req).await.unwrap();
        assert_eq!(picked.kind, ModelKind::Local);
    }

    #[tokio::test]
    async fn forbidden_models_are_skipped() {
        let pool = LlmPool::new(registry_two_local(), QuarantineConfig::default());
        let mut forbidden = HashSet::new();
        forbidden.insert(ProviderName::new("local:gemma"));
        let req = PickRequest {
            kind: TaskKind::SimpleEdit,
            forbidden,
            ..PickRequest::for_planning()
        };
        let picked = pool.pick(&req).await.unwrap();
        assert_eq!(picked.name.as_str(), "local:qwen");
    }
}
