//! jarvis-config — load Jarvis configuration from `jarvis.toml` + env vars.
//!
//! Precedence (lowest → highest): defaults → `jarvis.toml` → `.env` → process env.
//! Env-var interpolation in TOML values (`"${NAME}"`) is resolved against the merged env.

use figment::providers::{Env, Format, Toml};
use figment::Figment;
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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    #[serde(default = "default_web_enable")]
    pub enable: bool,
    #[serde(default = "default_web_addr")]
    pub addr: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enable: default_web_enable(),
            addr: default_web_addr(),
        }
    }
}

fn default_web_enable() -> bool {
    true
}
fn default_web_addr() -> String {
    "127.0.0.1:7878".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    /// "native" or "docker". Default: "native".
    #[serde(default = "default_backend")]
    pub default_backend: String,
    /// "none" | "egress_only" | "full". Default: "egress_only".
    #[serde(default = "default_net_policy")]
    pub default_net_policy: String,
    /// Docker image used by DockerSandbox.
    #[serde(default = "default_docker_image")]
    pub docker_image: String,
    /// Docker memory limit (e.g. "2g"). Optional.
    #[serde(default)]
    pub docker_memory: Option<String>,
    /// Docker CPU limit (fractional). Optional.
    #[serde(default)]
    pub docker_cpus: Option<f64>,
    /// Auto-pull the image at boot.
    #[serde(default)]
    pub docker_autopull: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            default_backend: default_backend(),
            default_net_policy: default_net_policy(),
            docker_image: default_docker_image(),
            docker_memory: None,
            docker_cpus: None,
            docker_autopull: false,
        }
    }
}

fn default_backend() -> String {
    "native".to_string()
}
fn default_net_policy() -> String {
    "egress_only".to_string()
}
fn default_docker_image() -> String {
    "alpine:3.20".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    #[serde(default = "default_addr")]
    pub addr: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            addr: default_addr(),
            data_dir: default_data_dir(),
        }
    }
}

fn default_addr() -> String {
    "127.0.0.1:7777".to_string()
}
fn default_data_dir() -> PathBuf {
    PathBuf::from(".jarvis")
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
}

fn default_priority() -> i32 {
    5
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesDecl {
    #[serde(default = "default_ctx")]
    pub ctx_len: u32,
    #[serde(default)]
    pub tool_calls: bool,
    #[serde(default)]
    pub json_schema: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default = "default_true")]
    pub supports_streaming: bool,
}

impl Default for CapabilitiesDecl {
    fn default() -> Self {
        Self {
            ctx_len: default_ctx(),
            tool_calls: false,
            json_schema: false,
            vision: false,
            supports_streaming: true,
        }
    }
}

fn default_ctx() -> u32 {
    8192
}
fn default_true() -> bool {
    true
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
#[serde(deny_unknown_fields)]
pub struct RoutingConfig {
    #[serde(default = "default_policy")]
    pub default_policy: String,
    #[serde(default)]
    pub monthly_remote_usd_cap: f64,
    #[serde(default)]
    pub escalation_chain: Vec<String>,
    #[serde(default = "default_quarantine_failures")]
    pub quarantine_after_failures: u32,
    #[serde(default = "default_quarantine_window")]
    pub quarantine_window_minutes: u32,
    #[serde(default = "default_quarantine_duration")]
    pub quarantine_duration_minutes: u32,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            default_policy: default_policy(),
            monthly_remote_usd_cap: 0.0,
            escalation_chain: Vec::new(),
            quarantine_after_failures: default_quarantine_failures(),
            quarantine_window_minutes: default_quarantine_window(),
            quarantine_duration_minutes: default_quarantine_duration(),
        }
    }
}

fn default_policy() -> String {
    "auto".to_string()
}
fn default_quarantine_failures() -> u32 {
    3
}
fn default_quarantine_window() -> u32 {
    10
}
fn default_quarantine_duration() -> u32 {
    15
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

    let toml_path = path.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("jarvis.toml"));

    let mut fig = Figment::new();

    // Layer 1: built-in defaults are encoded via serde defaults on each struct;
    //          merging the empty toml string makes figment emit them.
    fig = fig.merge(Toml::string(""));

    // Layer 2: jarvis.toml (interpolated). Optional.
    if toml_path.exists() {
        let raw = std::fs::read_to_string(&toml_path)?;
        let interpolated = interpolate_env(&raw)
            .map_err(|e| ConfigError::Interpolation(e.to_string()))?;
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
