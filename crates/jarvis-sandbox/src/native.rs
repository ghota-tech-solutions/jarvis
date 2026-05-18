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
            stdout: decode_stdio(&output.stdout),
            stderr: decode_stdio(&output.stderr),
            timed_out: false,
            backend: "native".to_string(),
        })
    }
}

fn shell_cmd(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        // `cmd.exe /U /C` forces every built-in (dir, type, where, set, …) to
        // emit Unicode (UTF-16LE) on its stdout, regardless of the active
        // console codepage. Combined with the UTF-16 decoder in `decode_stdio`
        // below, this is how we get clean accented characters on French/German
        // Windows where the default OEM codepage is CP850. We tried
        // `chcp 65001 && {cmd}` first — some Windows builds ignore the new
        // codepage for built-ins until the cmd process is fully reinitialised,
        // so `/U` is the only reliable knob.
        let mut c = Command::new("cmd.exe");
        c.arg("/U").arg("/C").arg(cmd);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(cmd);
        c
    }
}

/// Decode child-process stdout/stderr bytes.
/// * On Windows we requested UTF-16LE output via `cmd /U`, so the bytes are
///   little-endian u16 pairs that need decoding.
/// * Everywhere else, plain UTF-8 with lossy fallback.
fn decode_stdio(bytes: &[u8]) -> String {
    #[cfg(windows)]
    {
        // Strip an optional UTF-16LE BOM (FF FE).
        let stripped = if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
            &bytes[2..]
        } else {
            bytes
        };
        // Ensure even length — odd trailing byte means truncated final code unit.
        let units: Vec<u16> = stripped
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    }
    #[cfg(not(windows))]
    {
        String::from_utf8_lossy(bytes).into_owned()
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
