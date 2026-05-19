//! Client-side and server-side bearer-token auth helpers for the gRPC API.
//!
//! Both the daemon (server) and the CLI/TUI/desktop (clients) use the same
//! 256-bit token persisted by `jarvis-web::auth::AuthToken::load_or_create`
//! at `<data_dir>/web.token`. Centralizing the discovery + interceptor here
//! means new clients don't have to reinvent the auth wiring.
//!
//! Discovery order (first hit wins):
//!   1. `JARVIS_WEB_TOKEN` env var
//!   2. `JARVIS_DATA_DIR/web.token` (env)
//!   3. `./.jarvis/web.token` (cwd-relative — matches the daemon's default
//!      `data_dir = ".jarvis"`)
//!
//! On the server side, `ServerAuth` requires `authorization: Bearer
//! <token>` on every RPC. Comparison is constant-time. Health probes carry
//! the token too — every client auto-discovers it from the same file the
//! daemon writes (`<data_dir>/web.token`), so this is transparent in
//! practice.

use std::path::{Path, PathBuf};
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::{Request, Status};

/// File name relative to the daemon's `data_dir`.
pub const TOKEN_FILE: &str = "web.token";

/// Locate the bearer token using the standard discovery order. Returns
/// `None` if no token file is found and no env override is set.
pub fn discover_token() -> Option<String> {
    if let Ok(t) = std::env::var("JARVIS_WEB_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    if let Ok(dir) = std::env::var("JARVIS_DATA_DIR") {
        if let Some(t) = read_token_file(Path::new(&dir)) {
            return Some(t);
        }
    }
    read_token_file(Path::new(".jarvis"))
}

fn read_token_file(data_dir: &Path) -> Option<String> {
    let path: PathBuf = data_dir.join(TOKEN_FILE);
    let raw = std::fs::read_to_string(&path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Client-side interceptor — injects `authorization: Bearer <token>` on every
/// outgoing request. Use with `JarvisClient::with_interceptor(channel, ...)`.
#[derive(Clone)]
pub struct ClientAuth {
    header: MetadataValue<tonic::metadata::Ascii>,
}

impl ClientAuth {
    /// Build a client-auth interceptor from a token. Returns an error if the
    /// token contains characters that aren't valid in an HTTP header.
    pub fn new(token: &str) -> Result<Self, tonic::Status> {
        let value = format!("Bearer {token}")
            .parse::<MetadataValue<_>>()
            .map_err(|e| Status::invalid_argument(format!("token format: {e}")))?;
        Ok(Self { header: value })
    }
}

impl Interceptor for ClientAuth {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        req.metadata_mut()
            .insert("authorization", self.header.clone());
        Ok(req)
    }
}

/// Server-side interceptor — rejects RPCs that don't carry a matching token.
/// `disable_auth: true` flips it into a no-op + warning mode useful for
/// developer convenience. Public paths (`PUBLIC_METHODS`) always pass.
#[derive(Clone)]
pub struct ServerAuth {
    expected: Option<String>,
    disable_auth: bool,
}

impl ServerAuth {
    pub fn new(token: Option<String>, disable_auth: bool) -> Self {
        Self {
            expected: token,
            disable_auth,
        }
    }
}

impl Interceptor for ServerAuth {
    fn call(&mut self, req: Request<()>) -> Result<Request<()>, Status> {
        if self.disable_auth {
            return Ok(req);
        }
        let expected = match &self.expected {
            Some(t) => t,
            None => {
                return Err(Status::unauthenticated(
                    "daemon has no token configured but auth is required",
                ));
            }
        };
        let header = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let supplied = header.strip_prefix("Bearer ").unwrap_or("");
        if constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
            Ok(req)
        } else {
            Err(Status::unauthenticated("missing or invalid bearer token"))
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
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
    fn constant_time_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"x"));
    }

    #[test]
    fn server_auth_no_token_rejects_all() {
        let mut auth = ServerAuth::new(None, false);
        let req = Request::new(());
        assert!(auth.call(req).is_err());
    }

    #[test]
    fn server_auth_accepts_correct_token() {
        let mut auth = ServerAuth::new(Some("abc123".into()), false);
        let mut req = Request::new(());
        req.metadata_mut()
            .insert("authorization", "Bearer abc123".parse().unwrap());
        assert!(auth.call(req).is_ok());
    }

    #[test]
    fn server_auth_rejects_wrong_token() {
        let mut auth = ServerAuth::new(Some("abc123".into()), false);
        let mut req = Request::new(());
        req.metadata_mut()
            .insert("authorization", "Bearer wrong".parse().unwrap());
        assert!(auth.call(req).is_err());
    }

    #[test]
    fn server_auth_disable_auth_passes_all() {
        let mut auth = ServerAuth::new(None, true);
        let req = Request::new(());
        assert!(auth.call(req).is_ok());
    }

    #[test]
    fn discover_env_takes_priority() {
        // Save current env, set, test, restore.
        let prev = std::env::var("JARVIS_WEB_TOKEN").ok();
        // SAFETY: tests run single-threaded by default; env mutation here is
        // bounded to this test.
        unsafe {
            std::env::set_var("JARVIS_WEB_TOKEN", "envvalue");
        }
        let t = discover_token();
        assert_eq!(t.as_deref(), Some("envvalue"));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("JARVIS_WEB_TOKEN", v),
                None => std::env::remove_var("JARVIS_WEB_TOKEN"),
            }
        }
    }
}
