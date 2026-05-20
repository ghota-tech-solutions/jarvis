//! WSL2 sandbox — run commands inside a Windows Subsystem for Linux distro.
//!
//! Lets Windows users get Linux semantics (real `bash`, GNU coreutils,
//! Unix-style paths) without spinning up Docker. The workdir is mapped from
//! `C:\foo\bar` to `/mnt/c/foo/bar` automatically; arbitrary commands are
//! piped through `wsl.exe -d <distro> -- bash -lc "<cmd>"`.
//!
//! Isolation level: weaker than Docker (no kernel namespace, no resource
//! cgroup) but stronger than Native because the filesystem view differs and
//! Linux tooling can't accidentally call Windows-specific helpers. Suitable
//! for "I want my agent to think Linux without leaving Windows".

use crate::spec::{Sandbox, SandboxError, SandboxKind, SandboxOutput, SandboxSpec};
use async_trait::async_trait;
use std::path::Path;
#[cfg(windows)]
use std::process::Stdio;
#[cfg(windows)]
use tokio::process::Command;
#[cfg(windows)]
use tracing::debug;

/// WSL distro to invoke. Defaults to the user's default (`wsl.exe` without
/// `-d`); use `WslSandbox::with_distro` to pin a specific one.
#[derive(Debug, Clone, Default)]
pub struct WslSandbox {
    pub distro: Option<String>,
}

impl WslSandbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_distro(distro: impl Into<String>) -> Self {
        Self {
            distro: Some(distro.into()),
        }
    }
}

#[async_trait]
impl Sandbox for WslSandbox {
    fn kind(&self) -> SandboxKind {
        SandboxKind::Wsl2
    }

    async fn exec(&self, spec: SandboxSpec) -> Result<SandboxOutput, SandboxError> {
        #[cfg(not(windows))]
        {
            // On non-Windows hosts, WSL is irrelevant. Fall back to Native
            // semantics (sh -c). Documented in SandboxKind::Wsl2 doc.
            return crate::native::NativeSandbox.exec(spec).await;
        }

        #[cfg(windows)]
        {
            let wsl_path = wsl_workdir(&spec.workdir).ok_or_else(|| {
                SandboxError::Config(format!(
                    "workdir {:?} cannot be mapped to a WSL /mnt path",
                    spec.workdir
                ))
            })?;

            debug!(
                cmd = %jarvis_core::clip(&spec.cmd, 120),
                workdir = %wsl_path,
                distro = ?self.distro,
                "wsl2: exec"
            );

            let bash_inner = format!("cd '{}' && {}", shell_escape(&wsl_path), spec.cmd);
            let mut cmd = Command::new("wsl.exe");
            if let Some(d) = &self.distro {
                cmd.arg("-d").arg(d);
            }
            cmd.arg("--").arg("bash").arg("-lc").arg(&bash_inner);
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (k, v) in &spec.env {
                cmd.env(k, v);
            }

            let child = cmd.spawn()?;
            let output = match tokio::time::timeout(spec.timeout, child.wait_with_output()).await {
                Ok(o) => o?,
                Err(_) => {
                    return Ok(SandboxOutput {
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: format!("timeout after {}s", spec.timeout.as_secs()),
                        timed_out: true,
                        backend: "wsl2".to_string(),
                    });
                }
            };

            Ok(SandboxOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                timed_out: false,
                backend: "wsl2".to_string(),
            })
        }
    }
}

/// Convert a Windows path to its WSL2 `/mnt/<drive>/...` equivalent.
/// Returns None if the path has no recognisable Windows drive prefix or
/// looks like an already-WSL path.
pub fn wsl_workdir(p: &Path) -> Option<String> {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        // Already POSIX; assume it's a /mnt path or similar.
        return Some(s);
    }
    let bytes = s.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'/' || bytes[2] == b'\\') {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = &s[2..];
        let cleaned = rest.trim_start_matches('/');
        Some(format!("/mnt/{}/{}", drive, cleaned))
    } else {
        None
    }
}

/// Shell-escape a path for inclusion inside single quotes in a `bash -c`
/// invocation. The path must not itself contain newlines.
#[cfg(any(windows, test))]
fn shell_escape(p: &str) -> String {
    // Replace any single quote with the standard "'\''" escape.
    p.replace('\'', "'\\''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_windows_drive_to_mnt() {
        assert_eq!(
            wsl_workdir(Path::new("C:\\Users\\Ghota\\Project\\jarvis")).as_deref(),
            Some("/mnt/c/Users/Ghota/Project/jarvis"),
        );
        assert_eq!(
            wsl_workdir(Path::new("D:/data")).as_deref(),
            Some("/mnt/d/data"),
        );
    }

    #[test]
    fn passes_posix_path_through() {
        assert_eq!(
            wsl_workdir(Path::new("/mnt/c/already/posix")).as_deref(),
            Some("/mnt/c/already/posix"),
        );
    }

    #[test]
    fn rejects_relative_paths() {
        assert!(wsl_workdir(Path::new("relative/path")).is_none());
    }

    #[test]
    fn shell_escape_handles_quotes() {
        assert_eq!(shell_escape("a'b"), "a'\\''b");
        assert_eq!(shell_escape("simple"), "simple");
    }
}
