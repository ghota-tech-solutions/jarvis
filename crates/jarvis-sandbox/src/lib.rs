//! jarvis-sandbox — Sandbox trait + backends.
//!
//! Two backends ship in M3:
//!   - [`NativeSandbox`] — direct `tokio::process::Command`. No isolation.
//!   - [`DockerSandbox`] — ephemeral container per call (via bollard).
//!
//! Worktree creation lives in [`worktree`].

mod docker;
mod native;
mod spec;
pub mod worktree;
pub mod wsl2;

pub use docker::{DockerConfig, DockerSandbox};
pub use native::NativeSandbox;
pub use spec::{NetPolicy, Sandbox, SandboxError, SandboxKind, SandboxOutput, SandboxSpec};
pub use worktree::{Worktree, WorktreeManager};
pub use wsl2::WslSandbox;

/// Helper that turns config strings into a typed `DockerConfig`.
/// Lives here so the daemon doesn't need to depend on the bollard types directly.
pub fn docker_config_from_strings(
    image: &str,
    memory: Option<&str>,
    cpus: Option<f64>,
) -> DockerConfig {
    DockerConfig {
        image: image.to_string(),
        memory: memory.map(|s| s.to_string()),
        cpus,
    }
}
