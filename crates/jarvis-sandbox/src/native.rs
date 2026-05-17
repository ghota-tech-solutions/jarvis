//! Native sandbox — no OS isolation. Just `tokio::process::Command` in workdir.

use crate::spec::{Sandbox, SandboxError, SandboxKind, SandboxOutput, SandboxSpec};
use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;
use tracing::debug;

#[derive(Debug, Default, Clone)]
pub struct NativeSandbox;

#[async_trait]
impl Sandbox for NativeSandbox {
    fn kind(&self) -> SandboxKind {
        SandboxKind::Native
    }

    async fn exec(&self, spec: SandboxSpec) -> Result<SandboxOutput, SandboxError> {
        debug!(cmd = %clip(&spec.cmd, 120), workdir = %spec.workdir.display(), "native: exec");
        let mut cmd = shell_cmd(&spec.cmd);
        cmd.current_dir(&spec.workdir)
            .stdin(Stdio::null())
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
                    backend: "native".to_string(),
                });
            }
        };

        Ok(SandboxOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            timed_out: false,
            backend: "native".to_string(),
        })
    }
}

fn shell_cmd(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd.exe");
        c.arg("/C").arg(cmd);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(cmd);
        c
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    #[tokio::test]
    async fn echoes_back() {
        let dir = tempdir().unwrap();
        let s = NativeSandbox;
        let out = s
            .exec(SandboxSpec::new("echo hello", dir.path()))
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("hello"));
        assert_eq!(out.backend, "native");
    }

    #[tokio::test]
    async fn times_out_long_command() {
        let dir = tempdir().unwrap();
        let s = NativeSandbox;
        let cmd = if cfg!(windows) {
            "ping -n 10 127.0.0.1 > nul"
        } else {
            "sleep 10"
        };
        let out = s
            .exec(SandboxSpec::new(cmd, dir.path()).with_timeout(Duration::from_millis(200)))
            .await
            .unwrap();
        assert!(out.timed_out);
    }
}
