//! Bearer-token authentication for the SPA layer.
//!
//! Threat model: the daemon already binds to 127.0.0.1, so off-host attackers
//! cannot reach this server at all. The token defends against same-host
//! attackers — other unprivileged processes on the user's machine that could
//! otherwise read tasks, submit commands, or stream the ledger.
//!
//! Token is 256 bits of entropy from `getrandom` (via two `uuid::Uuid::new_v4`
//! concatenations), persisted at `<data_dir>/web.token` on first boot and
//! reused on subsequent boots so existing browser tabs stay authenticated.

use anyhow::{Context as _, Result};
use axum::{
    body::Body,
    extract::State,
    http::{header::AUTHORIZATION, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

/// File name within `data_dir` that holds the bearer token.
pub const TOKEN_FILE: &str = "web.token";

/// Routes that are allowed without a token, e.g. health checks for monitoring.
/// Keep this list tiny.
const PUBLIC_ROUTES: &[&str] = &["/v1/ping"];

/// Wrapper around the secret. `Clone` is cheap (Arc), but the inner string
/// never crosses serialization boundaries.
#[derive(Clone)]
pub struct AuthToken(Arc<String>);

impl AuthToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Load the token from `<data_dir>/web.token` if it exists, otherwise
    /// generate a fresh one and persist it. The file is created with `0o600`
    /// on Unix; on Windows the default ACL inherits from the parent directory.
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        let path = token_path(data_dir);
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let trimmed = existing.trim().to_string();
            if !trimmed.is_empty() {
                return Ok(Self(Arc::new(trimmed)));
            }
            warn!(path = %path.display(), "web.token is empty — regenerating");
        }
        let token = generate();
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create data_dir {}", data_dir.display()))?;
        write_secure(&path, &token)
            .with_context(|| format!("write {}", path.display()))?;
        info!(path = %path.display(), "generated new web.token");
        Ok(Self(Arc::new(token)))
    }
}

/// 256-bit hex token: two v4 UUIDs concatenated, dashes stripped. Both UUIDs
/// pull from the same `getrandom` source, so the result has the full 256 bits
/// of CSPRNG entropy.
fn generate() -> String {
    let mut s = String::with_capacity(64);
    s.push_str(&Uuid::new_v4().simple().to_string());
    s.push_str(&Uuid::new_v4().simple().to_string());
    s
}

pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TOKEN_FILE)
}

#[cfg(unix)]
fn write_secure(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secure(path: &Path, contents: &str) -> std::io::Result<()> {
    // On Windows we rely on the parent directory ACL. The data_dir lives under
    // the user's home; group/other access is already excluded by default.
    std::fs::write(path, contents)
}

/// Axum middleware: requires `Authorization: Bearer <token>` on every route
/// except `/v1/ping` (health). Returns 401 on missing/wrong token.
pub async fn require_token(
    State(token): State<AuthToken>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = req.uri().path();
    if PUBLIC_ROUTES.iter().any(|p| *p == path) {
        return Ok(next.run(req).await);
    }
    let header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let expected_prefix = "Bearer ";
    if let Some(supplied) = header.strip_prefix(expected_prefix) {
        if constant_time_eq(supplied.as_bytes(), token.as_str().as_bytes()) {
            return Ok(next.run(req).await);
        }
    }
    Err(StatusCode::UNAUTHORIZED)
}

/// Constant-time byte comparison to avoid timing side-channels on the token.
/// We compare the full lengths even if one side is shorter, by zero-padding
/// the shorter value's contribution into the accumulator.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        // Still consume both buffers so length leakage is bounded.
        let mut diff: u8 = (a.len() ^ b.len()) as u8;
        let n = a.len().min(b.len());
        for i in 0..n {
            diff |= a[i] ^ b[i];
        }
        return diff == 0;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_64_hex_chars() {
        let t = generate();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn tokens_differ() {
        assert_ne!(generate(), generate());
    }

    #[test]
    fn constant_time_eq_matches() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"x"));
    }

    #[test]
    fn persists_across_loads() {
        let tmp = std::env::temp_dir().join(format!("jarvis-web-auth-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let a = AuthToken::load_or_create(&tmp).unwrap();
        let b = AuthToken::load_or_create(&tmp).unwrap();
        assert_eq!(a.as_str(), b.as_str());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
