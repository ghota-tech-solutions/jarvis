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
        // Plain `cmd.exe /C` — we used to try `/U` to force UTF-16LE on the
        // built-ins, but that interpretation breaks **every external** program
        // (git, cargo, node, …) whose pipe output is already UTF-8 or CP-encoded
        // bytes. We now decode bytes after the fact in `decode_stdio` with a
        // UTF-8 → CP850 priority chain, which handles both worlds.
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

/// Decode child-process stdout/stderr bytes.
///
/// Priority chain:
///   1. **Strict UTF-8** — covers every modern external program on Windows
///      (git, cargo, node, python, …) plus all of Linux/macOS.
///   2. **CP850 fallback** (Windows only) — the default OEM codepage of
///      French/German/Spanish Windows consoles. `dir`, `type`, `where`,
///      `vol`, `chkdsk`, and other built-ins still write in this codepage.
///
/// Without the fallback, French `dir` output's `Répertoire` would arrive as
/// `R\x82pertoire` (CP850 `\x82` = é), `String::from_utf8` would fail, and
/// `from_utf8_lossy` would replace `\x82` with U+FFFD → `R�pertoire`.
fn decode_stdio(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    #[cfg(windows)]
    {
        decode_cp850(bytes)
    }
    #[cfg(not(windows))]
    {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Decode a byte slice assuming it is in CP850 (the default Windows OEM
/// codepage for many Western European locales). High bytes 0x80-0xFF are
/// mapped via the static table below; low bytes are ASCII pass-through.
#[cfg(windows)]
fn decode_cp850(bytes: &[u8]) -> String {
    /// CP850 → Unicode mapping for bytes 0x80-0xFF.
    /// Sourced from the IBM/Microsoft CP850 specification.
    const CP850_HIGH: [char; 128] = [
        // 0x80
        'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å',
        // 0x90
        'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', 'ø', '£', 'Ø', '×', 'ƒ',
        // 0xA0
        'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '®', '¬', '½', '¼', '¡', '«', '»',
        // 0xB0
        '░', '▒', '▓', '│', '┤', 'Á', 'Â', 'À', '©', '╣', '║', '╗', '╝', '¢', '¥', '┐',
        // 0xC0
        '└', '┴', '┬', '├', '─', '┼', 'ã', 'Ã', '╚', '╔', '╩', '╦', '╠', '═', '╬', '¤',
        // 0xD0
        'ð', 'Ð', 'Ê', 'Ë', 'È', 'ı', 'Í', 'Î', 'Ï', '┘', '┌', '█', '▄', '¦', 'Ì', '▀',
        // 0xE0
        'Ó', 'ß', 'Ô', 'Ò', 'õ', 'Õ', 'µ', 'þ', 'Þ', 'Ú', 'Û', 'Ù', 'ý', 'Ý', '¯', '´',
        // 0xF0
        '\u{00AD}', '±', '‗', '¾', '¶', '§', '÷', '¸', '°', '¨', '·', '¹', '³', '²', '■',
        '\u{00A0}',
    ];
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b < 0x80 {
            out.push(b as char);
        } else {
            out.push(CP850_HIGH[(b - 0x80) as usize]);
        }
    }
    out
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

    #[test]
    fn decode_stdio_picks_utf8_when_valid() {
        let s = decode_stdio("Répertoire".as_bytes());
        assert_eq!(s, "Répertoire");
    }

    #[cfg(windows)]
    #[test]
    fn decode_stdio_falls_back_to_cp850() {
        // CP850 bytes for "Répertoire": R(0x52) é(0x82) p(0x70) e(0x65) r(0x72)
        // t(0x74) o(0x6F) i(0x69) r(0x72) e(0x65)
        let cp850 = b"R\x82pertoire";
        let s = decode_stdio(cp850);
        assert_eq!(s, "Répertoire");
    }

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
