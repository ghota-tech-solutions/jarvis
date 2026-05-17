use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Selects which backend a task uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxKind {
    #[default]
    Native,
    Docker,
}

impl SandboxKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Docker => "docker",
        }
    }
}

impl std::str::FromStr for SandboxKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "native" | "" => Ok(Self::Native),
            "docker" => Ok(Self::Docker),
            other => Err(format!("unknown sandbox kind: {other}")),
        }
    }
}

/// Network policy. Applies to `DockerSandbox`; `NativeSandbox` is always unrestricted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum NetPolicy {
    /// Air-gapped — `--network=none`. Use for pure transformation tasks.
    None,
    /// Egress only — Docker bridge (outbound works, no inbound). Default.
    #[default]
    EgressOnly,
    /// Full network — `--network=bridge` with no extra confinement. Equivalent
    /// to default Docker; here for API parity with future allowlist mode.
    Full,
}

impl NetPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::EgressOnly => "egress_only",
            Self::Full => "full",
        }
    }
}

impl std::str::FromStr for NetPolicy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "egress_only" | "egress" | "" => Ok(Self::EgressOnly),
            "full" => Ok(Self::Full),
            other => Err(format!("unknown net policy: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SandboxSpec {
    /// The shell command. Interpreted by `/bin/sh -c` on Linux, `cmd /C` on
    /// Windows native, and `/bin/sh -c` inside Docker containers.
    pub cmd: String,
    /// Working directory.
    pub workdir: PathBuf,
    /// Hard cap on wall time.
    pub timeout: Duration,
    /// Network policy. Only honored by Docker; native always has full access.
    pub net: NetPolicy,
    /// Extra env vars merged into the child environment.
    pub env: HashMap<String, String>,
}

impl SandboxSpec {
    pub fn new(cmd: impl Into<String>, workdir: impl Into<PathBuf>) -> Self {
        Self {
            cmd: cmd.into(),
            workdir: workdir.into(),
            timeout: Duration::from_secs(60),
            net: NetPolicy::default(),
            env: HashMap::new(),
        }
    }
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
    pub fn with_net(mut self, n: NetPolicy) -> Self {
        self.net = n;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    /// Backend that ran this (`"native"` or `"docker"`).
    pub backend: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bollard: {0}")]
    Bollard(#[from] bollard::errors::Error),
    #[error("docker daemon unreachable: {0}")]
    DockerUnavailable(String),
    #[error("cancelled")]
    Cancelled,
    #[error("image pull failed: {0}")]
    ImagePull(String),
    #[error("config: {0}")]
    Config(String),
    #[error("{0}")]
    Other(String),
}

#[async_trait]
pub trait Sandbox: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> SandboxKind;
    async fn exec(&self, spec: SandboxSpec) -> Result<SandboxOutput, SandboxError>;
}
