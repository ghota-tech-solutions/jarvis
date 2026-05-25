//! jarvis-config — load Jarvis configuration from `jarvis.toml` + env vars.
//!
//! Precedence (lowest → highest): defaults → `jarvis.toml` → `.env` → process env.
//! Env-var interpolation in TOML values (`"${NAME}"`) is resolved against the merged env.

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

mod interpolate;

pub use interpolate::interpolate_env;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub hooks: HooksConfig,
    #[serde(default)]
    pub validation: ValidationConfig,
    #[serde(default)]
    pub agent: AgentConfig,
}

/// § T1.3 — agent loop tunables that don't fit into routing / sandbox /
/// validation buckets.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// When true, the system prompt's tool catalog is rendered compactly
    /// (names + descriptions only). The agent must call `search_tools(query)`
    /// to retrieve full JSON-schema args. Saves ~3–5 k tokens per turn on
    /// registries with 12+ tools but costs one extra round-trip the first
    /// time the model uses an unfamiliar tool. Default: false (eager —
    /// preserves pre-T1.3 behaviour).
    pub lazy_tool_catalog: bool,
}

/// M11.S6 + M12.S4 — verdict validation. When the agent emits `done`,
/// optionally ask either a second LLM (M11.S6) or a full reviewer
/// sub-agent (M12.S4) "did this actually accomplish the goal?". If the
/// validator says NO, the agent gets a continuation event and keeps
/// working instead of declaring success.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ValidationConfig {
    /// Master switch. Off by default — costs an extra LLM call per `done`.
    pub enabled: bool,
    /// Model to use for validation. Empty = pick a different one than the
    /// task's primary model (best for cross-checking; defaults to the
    /// highest-priority remote, or fallback to highest local).
    pub model: String,
    /// Cap the number of validator-driven continuations per task so a stuck
    /// validator can't loop forever. Default 1 — one chance to retry.
    pub max_validations: u32,
    /// M12.S4 : when true, the validator fires a full reviewer sub-agent
    /// (read-only tools: fs_read / grep / glob / web_search) instead of a
    /// plain LLM call. The sub-agent can actually inspect the workdir and
    /// catch issues a stateless LLM call misses (failed compilation, bad
    /// patches, etc.). Costs more tokens + adds latency, so off by default.
    pub use_subagent: bool,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            max_validations: 1,
            use_subagent: false,
        }
    }
}

/// Hooks let the user wire arbitrary verification commands to fire after the
/// agent runs a tool. The canonical use case is auto-`cargo check` after every
/// `apply_patch` / `fs_write` so the agent can't lie about success — if the
/// hook fails, the captured output is fed back as an observation on the next
/// turn.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HooksConfig {
    /// Runs BEFORE a tool's `invoke()` (the agent's decision to call this tool
    /// has just landed). Use for pre-flight checks, secret redaction, or
    /// snapshotting state. Multiple hooks can match the same tool — they all
    /// run, in declaration order.
    #[serde(default)]
    pub pre_tool: Vec<HookConfig>,
    /// Runs AFTER a tool's `tool_result` lands in the ledger (successful
    /// invocation, regardless of `is_error` inside the payload). Multiple
    /// hooks can match the same tool — they all run, in declaration order.
    #[serde(default)]
    pub post_tool: Vec<HookConfig>,
    /// Runs ONLY when a tool fails to execute at all (the `invoke()` call
    /// returns `Err` — e.g. sandbox error, schema validation rejection).
    /// Use for incident logging, alerting, or auto-replan triggers.
    /// Multiple hooks can match the same tool — they all run, in declaration
    /// order.
    #[serde(default)]
    pub on_error: Vec<HookConfig>,
}

/// One entry in `[[hooks.pre_tool]]`, `[[hooks.post_tool]]`, or
/// `[[hooks.on_error]]`. The shape is identical across phases — the phase is
/// determined by which list the entry lives in.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookConfig {
    /// Regex matched against the tool name (e.g. `apply_patch|fs_write`).
    pub r#match: String,
    /// Shell command to run. Resolved by `jarvis-sandbox::native::shell_cmd` so
    /// it inherits the Windows `chcp 65001` + UTF-8 fallback chain.
    pub cmd: String,
    /// Where to run. Defaults to the task's workdir.
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    /// Cap before the command is killed; default 60 s.
    #[serde(default = "default_hook_timeout")]
    pub timeout_s: u64,
    /// Short label shown in the ledger / UI ("cargo check", "lint", etc.).
    /// Defaults to the first word of `cmd`.
    #[serde(default)]
    pub label: Option<String>,
}

/// Backwards-compatible alias for code that still spells the old name.
pub type PostToolHook = HookConfig;

fn default_hook_timeout() -> u64 {
    60
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// Multi-entry, keyed by server name. Each entry spawns one MCP subprocess.
    #[serde(default)]
    pub servers: HashMap<String, McpServer>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    /// Disable without removing the section.
    #[serde(default = "default_true")]
    pub enable: bool,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub workdir: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebConfig {
    pub enable: bool,
    pub addr: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enable: true,
            addr: "127.0.0.1:7878".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxConfig {
    /// "native" or "docker". Default: "native".
    pub default_backend: String,
    /// "none" | "egress_only" | "full". Default: "egress_only".
    pub default_net_policy: String,
    /// "read_only" | "workspace_write" | "danger_full_access". Default:
    /// "workspace_write". Per-task override available at submit time.
    pub default_mode: String,
    /// Docker image used by DockerSandbox.
    pub docker_image: String,
    /// Docker memory limit (e.g. "2g"). Optional.
    pub docker_memory: Option<String>,
    /// Docker CPU limit (fractional). Optional.
    pub docker_cpus: Option<f64>,
    /// Auto-pull the image at boot.
    pub docker_autopull: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            default_backend: "native".to_string(),
            default_net_policy: "egress_only".to_string(),
            default_mode: "workspace_write".to_string(),
            docker_image: "alpine:3.20".to_string(),
            docker_memory: None,
            docker_cpus: None,
            docker_autopull: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub addr: String,
    pub data_dir: PathBuf,
    /// When false (default) the daemon refuses to start if `addr` is non-loopback.
    /// Set to true to opt into LAN/wildcard binds (you'd better have auth on).
    #[serde(default)]
    pub bind_lan: bool,
    /// When true, gRPC + jarvis-web accept requests without a bearer token.
    /// Use only in airtight local-dev situations; logs a warning on boot.
    #[serde(default)]
    pub disable_auth: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:7777".to_string(),
            data_dir: PathBuf::from(".jarvis"),
            bind_lan: false,
            disable_auth: false,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidersConfig {
    /// Multi-entry, keyed by provider name. M1 only uses one local provider.
    #[serde(default)]
    pub local: HashMap<String, LocalProvider>,
    #[serde(default)]
    pub remote: HashMap<String, RemoteProvider>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalProvider {
    pub url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub capabilities: CapabilitiesDecl,
    /// § C.M-B — tool-call dialect for this model. Defaults to `Json`.
    /// Set to `"gemma4_strict"` for any Gemma family model to get the
    /// tighter system prompt with explicit anti-drift recovery clause.
    #[serde(default)]
    pub tool_dialect: jarvis_core::ToolDialect,
    /// Enable Gemma 4 reflection / thinking mode — prepends `<|think|>`
    /// to the system prompt so the model reasons in a dedicated channel
    /// before its answer. Off by default.
    #[serde(default)]
    pub thinking: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteProvider {
    pub url: String,
    pub model: String,
    pub api_key: String,
    #[serde(default)]
    pub cost_per_mtok_in: f64,
    #[serde(default)]
    pub cost_per_mtok_out: f64,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub capabilities: CapabilitiesDecl,
    /// § C.M-B — tool-call dialect for this model. Defaults to `Json`.
    #[serde(default)]
    pub tool_dialect: jarvis_core::ToolDialect,
    /// Enable Gemma 4 reflection / thinking mode. Off by default.
    #[serde(default)]
    pub thinking: bool,
}

fn default_priority() -> i32 {
    5
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilitiesDecl {
    pub ctx_len: u32,
    pub tool_calls: bool,
    pub json_schema: bool,
    pub vision: bool,
    pub supports_streaming: bool,
}

impl Default for CapabilitiesDecl {
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

impl From<CapabilitiesDecl> for jarvis_core::Capabilities {
    fn from(d: CapabilitiesDecl) -> Self {
        Self {
            ctx_len: d.ctx_len,
            tool_calls: d.tool_calls,
            json_schema: d.json_schema,
            vision: d.vision,
            supports_streaming: d.supports_streaming,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingConfig {
    pub default_policy: String,
    pub monthly_remote_usd_cap: f64,
    pub escalation_chain: Vec<String>,
    pub quarantine_after_failures: u32,
    pub quarantine_window_minutes: u32,
    pub quarantine_duration_minutes: u32,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            default_policy: "auto".to_string(),
            monthly_remote_usd_cap: 0.0,
            escalation_chain: Vec::new(),
            quarantine_after_failures: 3,
            quarantine_window_minutes: 10,
            quarantine_duration_minutes: 15,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config: {0}")]
    Figment(Box<figment::Error>),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("env interpolation: {0}")]
    Interpolation(String),
}

impl From<figment::Error> for ConfigError {
    fn from(e: figment::Error) -> Self {
        Self::Figment(Box::new(e))
    }
}

/// Load config from `path` (TOML), apply env-var interpolation on string values,
/// merge env-var overrides under `JARVIS_*` prefix.
///
/// If `path` is `None`, the loader looks for `./jarvis.toml`, falling back to defaults.
pub fn load(path: Option<&Path>) -> Result<Config, ConfigError> {
    // Best-effort .env — never fatal.
    let _ = dotenvy::dotenv();

    let toml_path = path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("jarvis.toml"));

    let mut fig = Figment::new();

    // Layer 1: built-in defaults are encoded via serde defaults on each struct;
    //          merging the empty toml string makes figment emit them.
    fig = fig.merge(Toml::string(""));

    // Layer 2: jarvis.toml (interpolated). Optional.
    if toml_path.exists() {
        let raw = std::fs::read_to_string(&toml_path)?;
        let interpolated =
            interpolate_env(&raw).map_err(|e| ConfigError::Interpolation(e.to_string()))?;
        fig = fig.merge(Toml::string(&interpolated));
    }

    // Layer 3: env-var overrides like JARVIS_DAEMON_ADDR=...
    // Skip env vars used by other subsystems (logging, etc.) so figment doesn't
    // try to deserialize them into Config.
    fig = fig.merge(
        Env::prefixed("JARVIS_")
            .ignore(&["LOG", "CONFIG", "DAEMON_URL"])
            .split("_"),
    );

    Ok(fig.extract()?)
}
