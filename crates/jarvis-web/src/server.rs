//! Axum server boot for the new SPA layer.
//!
//! Runs on its own port, in parallel to the legacy HTMX UI in
//! `jarvis-daemon/src/web/`. Coexistence is the M6 design: the legacy keeps
//! serving on `cfg.web.addr` (default 7878), the new SPA layer serves on
//! `spa_addr` (default 7879, override via env `JARVIS_SPA_ADDR`).
//!
//! Every route except `/v1/ping` requires a Bearer token (see `auth.rs`).

use anyhow::Context as _;
use axum::{middleware, routing::get, Json, Router};
use serde::Serialize;
use std::path::Path;
use tracing::info;

use crate::auth::{self, AuthToken};
use crate::VERSION;

/// Default bind address when `JARVIS_SPA_ADDR` is unset.
pub const DEFAULT_SPA_ADDR: &str = "127.0.0.1:7879";

/// Resolve the SPA bind address: env override, or fallback to default.
pub fn resolve_addr() -> String {
    std::env::var("JARVIS_SPA_ADDR").unwrap_or_else(|_| DEFAULT_SPA_ADDR.to_string())
}

/// Build the axum router for the SPA layer.
///
/// Auth middleware is applied to every route; `/v1/ping` is explicitly
/// whitelisted inside the middleware so health probes work without a token.
pub fn router(token: AuthToken) -> Router {
    Router::new()
        .route("/v1/ping", get(ping))
        .layer(middleware::from_fn_with_state(
            token.clone(),
            auth::require_token,
        ))
        .with_state(token)
}

#[derive(Serialize)]
struct PingResponse {
    ok: bool,
    crate_: &'static str,
    version: &'static str,
}

async fn ping() -> Json<PingResponse> {
    Json(PingResponse {
        ok: true,
        crate_: "jarvis-web",
        version: VERSION,
    })
}

/// Run the SPA axum server until the future is dropped or the listener errors.
///
/// `data_dir` is where the bearer token is persisted (`<data_dir>/web.token`).
pub async fn serve(addr: String, data_dir: &Path) -> anyhow::Result<()> {
    let token = AuthToken::load_or_create(data_dir)
        .context("load or create web auth token")?;
    let app = router(token);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind jarvis-web SPA on {addr}"))?;
    info!(
        %addr,
        token_path = %auth::token_path(data_dir).display(),
        "jarvis-web SPA listening"
    );
    axum::serve(listener, app)
        .await
        .context("jarvis-web SPA server")
}
